//! The Strata Terminal: Strata's `strata` command, which Strata's own
//! `install.ps1` puts into the user's folder. Only this module knows where
//! it lives, how it is called and what it answers; jobs ask it for the
//! commands they run and hand it the output to read.
//!
//! Every command names the folder the run configs live in
//! (`STRATA_INSTALL_DIR`): without it the Terminal looks for them in older
//! installs first, an old Strata folder from source among them.

use std::path::{Path, PathBuf};

use serde_json::Value;
use strata_pc_protocol::{
    DocumentedVariable, NeverRule, PresetState, SpeedSwitch, StrataOptions, StrataPreset,
    StrataPresets, StrataTune, TuneMode, TuneSpeed, TuneUse, TunerOption, TunerTry,
};

use crate::command::CommandSpec;

/// The install script's folder for the Terminal itself, a link to the
/// version installed last.
const CURRENT: [&str; 2] = ["terminal", "current"];
/// The Strata folder with the run configs, beside the engines, where
/// Strata's desktop app keeps them too.
const INSTALL: &str = "install";
/// The `strata` command in the Terminal's own files.
const PROGRAM: [&str; 2] = ["bin", "strata.cmd"];
/// Prints the tuner's option table (`strata_control/tune_knobs.py`), the
/// speed switches (`strata_control/speed_switches.py`) with the run configs
/// in `STRATA_INSTALL_DIR` each applies to, whether Strata's hardware
/// detection finds an RX 6000 card (setup's `is_rdna2`) and, for the run
/// config named as its argument, what its engine prints for `--help`, run
/// the way Strata's own tuner runs it (`strata_control/tuning.py`). The
/// Terminal has no command for any of them. Each part fails on its own.
/// Its first line names it for the tests' fake commands, which otherwise
/// answer it as setup.
const OPTIONS_SCRIPT: &str = r#"# options
import importlib.util, json, os, subprocess, sys
from pathlib import Path
def plain(v):
    if isinstance(v, (tuple, list, set, frozenset)):
        return [plain(x) for x in v]
    if isinstance(v, dict):
        return {str(a): plain(b) for a, b in v.items()}
    return v
out = {}
try:
    from strata_control import tune_knobs as k
    out["tuner"] = {"bounds": plain(k.BOUNDS), "never": plain(k.NEVER),
        "answer_of_flag": plain(k.ANSWER_OF_FLAG), "bool_flags": sorted(k.BOOL_FLAGS),
        "arms": [{"label": a.label, "answer": a.answer, "set_args": plain(a.set_args),
                  "env": plain(a.env), "families": plain(a.families)} for a in k.ARMS]}
except Exception as e:
    out["tuner_error"] = str(e)
gpus = []
try:
    spec = importlib.util.spec_from_file_location("strata_mcp_options", Path.cwd() / "tools" / "strata_mcp.py")
    mcp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mcp)
    s = mcp.Strata(os.environ["STRATA_INSTALL_DIR"], Path.cwd())
    gpus = list(s.hardware().get("gpus") or [])
    out["rdna2"] = any(s.setup_module().is_rdna2(g) for g in gpus)
except Exception as e:
    out["hardware_error"] = str(e)
try:
    from strata_control import speed_switches
    configs = {}
    for f in Path(os.environ["STRATA_INSTALL_DIR"]).glob("strata-*.json"):
        name = f.name[len("strata-"):-len(".json")]
        if name and not name.endswith(".shared-settings"):
            try:
                configs[name] = json.loads(f.read_text(encoding="utf-8-sig"))
            except Exception:
                pass
    backend = None
    try:
        from strata_control import engine_component
        rep = engine_component.status()
        if rep.get("usable"):
            backend = (rep.get("engine") or {}).get("backend")
    except Exception:
        pass
    out["switches"] = [{"label": sw.label, "effect": sw.effect, "default": sw.default, "env": dict(sw.env),
        "models": sorted(m for m, c in configs.items() if speed_switches.applies(sw, c, gpus, backend))} for sw in speed_switches.SWITCHES]
except Exception as e:
    out["switches_error"] = str(e)
if len(sys.argv) > 1:
    try:
        from strata_control import engine_component, tuning
        cfg = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8-sig"))
        exe = Path(engine_component.expand(cfg["exe"], cfg, full=False))
        p = subprocess.run([*tuning.engine_command(exe), "--help"], capture_output=True,
                           text=True, timeout=20, env=tuning.engine_env(exe))
        out["engine_help"] = f"{p.stdout or ''}\n{p.stderr or ''}"
    except Exception as e:
        out["engine_error"] = str(e)
print(json.dumps(out))
"#;
/// Where `strata update` keeps the engines and newer Terminal files
/// ("Control"), beside `install` with Strata's defaults.
const COMPONENTS: &str = "components";

/// The Strata Terminal in its folder (`strata_home`).
#[derive(Clone, Debug)]
pub struct Terminal {
    home: PathBuf,
    components: PathBuf,
}

impl Terminal {
    /// The Terminal in `home`, with Strata's components beside it.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let components = home.join(COMPONENTS);
        Self { home, components }
    }

    pub fn of(cfg: &crate::Config) -> Self {
        Self {
            home: cfg.strata_home.clone(),
            components: cfg.strata_components.clone(),
        }
    }

    /// Strata's component folder, with its Control and engine folders.
    pub fn components(&self) -> &Path {
        &self.components
    }

    /// The Terminal as the install script left it.
    fn current(&self) -> PathBuf {
        CURRENT
            .iter()
            .fold(self.home.clone(), |p, part| p.join(part))
    }

    /// The Terminal's own files that run: `strata.py`, `setup.py`, `serve`,
    /// `data`. After `strata update` installed newer ones ("Control" in
    /// `components\control`), those, as the `strata` command hands over to
    /// them (`strata_cli/handover.py`, `find_target`).
    pub fn root(&self) -> PathBuf {
        let current = self.current();
        self.updated_root(&current).unwrap_or(current)
    }

    /// The newer Terminal files `components\control\active.json` names,
    /// if they are whole.
    fn updated_root(&self, current: &Path) -> Option<PathBuf> {
        let base = self.components.join("control");
        let pointer: Value =
            serde_json::from_str(&std::fs::read_to_string(base.join("active.json")).ok()?).ok()?;
        let name = pointer["folder"].as_str()?;
        let plain = |c: char| c.is_ascii_alphanumeric() || "._+-".contains(c);
        if name.is_empty() || name == "." || name == ".." || !name.chars().all(plain) {
            return None;
        }
        let folder = base.join(name);
        let have = version_key(&read_version(current)?)?;
        let want = version_key(
            &read_version(&folder).or_else(|| text(&pointer["version"]).map(String::from))?,
        )?;
        let whole = folder.join("python").join("python.exe").is_file()
            && folder.join("strata.py").is_file()
            && folder.join("strata_control").is_dir();
        (want > have && whole).then_some(folder)
    }

    /// The Python of the Terminal's files that run ([`root`](Self::root)),
    /// the one `strata serve` runs Strata with.
    pub fn python(&self) -> PathBuf {
        self.root().join("python").join("python.exe")
    }

    /// The `strata` command; it hands over to newer files by itself.
    pub fn program(&self) -> PathBuf {
        Self::program_in(&self.current())
    }

    /// The `strata` command among the Terminal's own files in `root`.
    pub fn program_in(root: &Path) -> PathBuf {
        PROGRAM
            .iter()
            .fold(root.to_path_buf(), |p, part| p.join(part))
    }

    pub fn is_installed(&self) -> bool {
        self.program().is_file()
    }

    /// The Strata folder: where setup writes the run configs
    /// (`strata-<id>.json`).
    pub fn install_dir(&self) -> PathBuf {
        self.home.join(INSTALL)
    }

    /// The build of the files that run, as their `VERSION` file says;
    /// `strata --version` prints it as the build.
    pub fn build(&self) -> Option<String> {
        read_version(&self.root())
    }

    /// The Strata commit the files that run were built from (`COMMIT`,
    /// which `packaging/package_terminal.py` writes).
    pub fn commit(&self) -> Option<String> {
        first_line(&self.root().join("COMMIT"))
    }

    /// `strata --version`; read it with [`version_text`].
    pub fn version(&self) -> CommandSpec {
        self.command(&["--version"])
    }

    /// The engine and the Terminal's files from the signed feed, each only
    /// after a test start. Each line is an [`update_event`] until the last,
    /// the answer: read the output with [`updated`].
    pub fn update(&self) -> CommandSpec {
        self.command(&["update", "--output", "jsonl"])
    }

    /// What `strata update` would change, without downloading. Read the
    /// output with [`update_available`].
    pub fn check_update(&self) -> CommandSpec {
        self.command(&["update", "--check", "--output", "json"])
    }

    /// Strata's standard benchmark against the Strata serving on `url`,
    /// with the key in `STRATA_API_KEY`, its report in `out` (a new
    /// folder). Read the output with [`bench_speeds`].
    pub fn bench(&self, url: &str, key: Option<&str>, out: &Path) -> CommandSpec {
        let out = out.display().to_string();
        let mut spec = self.command(&[
            "bench", "run", "--url", url, "--out", &out, "--output", "json",
        ]);
        if let Some(key) = key {
            spec.env.push(("STRATA_API_KEY".into(), key.into()));
        }
        spec
    }

    /// Strata's auto-tuner for the run config `config`, for speed only: it
    /// never tries settings that change answers or are not repeatable. It
    /// writes its progress to `status` and saves its result as the active
    /// preset; read the output with [`tune_result`], the status with
    /// [`tune_progress`].
    pub fn tune(
        &self,
        config: &Path,
        mode: TuneMode,
        usage: TuneUse,
        status: &Path,
    ) -> CommandSpec {
        let config = config.display().to_string();
        let status = status.display().to_string();
        let mode = match mode {
            TuneMode::Quick => "quick",
            TuneMode::Thorough => "thorough",
        };
        let usage = match usage {
            TuneUse::Coding => "coding",
            TuneUse::Chat => "chat",
        };
        self.command(&[
            "tune",
            "auto",
            &config,
            "--goal",
            "speed",
            "--mode",
            mode,
            "--use",
            usage,
            "--status-file",
            &status,
            "--output",
            "json",
        ])
    }

    /// Makes the preset that was active before `preset` the active one
    /// again (none: the standard settings); read the output with
    /// [`preset_answer`].
    pub fn undo_preset(&self, preset: &str) -> CommandSpec {
        self.command(&["presets", "undo", preset, "--output", "json"])
    }

    /// Switches the active preset of run config `config`'s model and
    /// choices off: back to the standard settings, the preset stays saved;
    /// read the output with [`preset_answer`].
    pub fn preset_off(&self, config: &Path) -> CommandSpec {
        let config = config.display().to_string();
        self.command(&["presets", "off", &config, "--output", "json"])
    }

    /// Makes `preset` the active one for its model and choices, keeping the
    /// one before for undo; read the output with [`preset_answer`].
    pub fn apply_preset(&self, preset: &str) -> CommandSpec {
        self.command(&["presets", "apply", preset, "--output", "json"])
    }

    /// The presets of the model and choices of run config `config`; read
    /// the output with [`read_presets`].
    pub fn presets_status(&self, config: &Path) -> CommandSpec {
        let config = config.display().to_string();
        self.command(&["presets", "status", &config, "--output", "json"])
    }

    /// What makes up a running Strata: every program from the Terminal's
    /// folder (its Python) and the engines beside it.
    pub fn processes(&self) -> Vec<String> {
        let mut entries = vec![crate::platform::folder_entry(&self.home)];
        if !self.components.starts_with(&self.home) {
            entries.push(crate::platform::folder_entry(&self.components));
        }
        entries
    }

    /// Strata's install script from `installer`, into this folder. It
    /// checks the archive's SHA-256 and the release's signed feed itself.
    pub fn install(&self, installer: &str) -> CommandSpec {
        let script = format!("irm '{}' | iex", installer.replace('\'', "''"));
        CommandSpec {
            program: "powershell".into(),
            args: [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ]
            .map(String::from)
            .to_vec(),
            dir: None,
            env: vec![("STRATA_DATA_DIR".into(), self.home.display().to_string())],
        }
    }

    /// `strata <args>`.
    pub fn command(&self, args: &[&str]) -> CommandSpec {
        CommandSpec {
            program: self.program(),
            args: args.iter().map(|a| a.to_string()).collect(),
            dir: Some(self.root()),
            env: vec![self.install_env()],
        }
    }

    /// Which engine folder of the component folder Strata starts on this
    /// PC, for `backend` if given; never downloads. Read the output with
    /// [`engine_root`].
    pub fn components_engine(&self, backend: Option<&str>) -> CommandSpec {
        let mut args = vec!["components", "engine", "--output", "json"];
        if let Some(backend) = backend {
            args.extend(["--backend", backend]);
        }
        self.command(&args)
    }

    /// The engine for this PC from the signed feed; the Terminal itself
    /// stays as it is. Each line is an [`update_event`] until the last, the
    /// answer: read the output with [`engine_installed`].
    pub fn install_engine(&self) -> CommandSpec {
        self.command(&["update", "--no-control", "--output", "jsonl"])
    }

    /// Setup without questions in its install mode, the way `strata launch`
    /// and Strata's desktop app install a model: download and prepare only,
    /// with no engine fetch, no packages and no start. It prints setup's
    /// usual steps and progress and writes the run config.
    pub fn setup(&self, args: Vec<String>) -> CommandSpec {
        let root = self.root();
        let mut all = vec![root.join("setup.py").display().to_string()];
        all.extend(args);
        CommandSpec {
            program: root.join("python").join("python.exe"),
            args: all,
            dir: Some(root),
            env: vec![
                ("STRATA_CONTROL_INSTALL".into(), "1".into()),
                self.install_env(),
                ("PYTHONNOUSERSITE".into(), "1".into()),
            ],
        }
    }

    /// Prints Strata's options, with the help of the engine that run config
    /// `config` starts; read the output with [`read_options`].
    pub fn options(&self, config: Option<&Path>) -> CommandSpec {
        let root = self.root();
        let mut args = vec!["-c".to_string(), OPTIONS_SCRIPT.to_string()];
        args.extend(config.map(|c| c.display().to_string()));
        CommandSpec {
            program: root.join("python").join("python.exe"),
            args,
            dir: Some(root),
            env: vec![self.install_env(), ("PYTHONNOUSERSITE".into(), "1".into())],
        }
    }

    /// Prints Strata's catalog for this PC ([`crate::catalog::SCRIPT`]),
    /// with the run configs in `install` and the models in `data`.
    pub fn catalog(&self, install: &Path, data: &Path) -> CommandSpec {
        let root = self.root();
        CommandSpec {
            program: root.join("python").join("python.exe"),
            args: vec![
                "-c".into(),
                crate::catalog::SCRIPT.into(),
                install.display().to_string(),
                data.display().to_string(),
            ],
            dir: Some(root),
            env: vec![self.install_env(), ("PYTHONNOUSERSITE".into(), "1".into())],
        }
    }

    /// The arguments to start the `strata` command with, so that it serves
    /// `config` in the foreground on `host` and `port`; the key comes from
    /// the run config.
    pub fn serve_args(config: &Path, host: &str, port: &str) -> Vec<String> {
        [
            "serve",
            "--profile",
            &config.display().to_string(),
            "--host",
            host,
            "--port",
            port,
        ]
        .map(String::from)
        .to_vec()
    }

    /// `args` with `host` in place of the host they name, when they are
    /// [`serve_args`](Self::serve_args); others (an old Strata folder's own
    /// arguments) stay as they are.
    pub fn on_host(args: &[String], host: &str) -> Vec<String> {
        let mut args = args.to_vec();
        if args.first().map(String::as_str) == Some("serve")
            && let Some(at) = args.iter().position(|a| a == "--host")
            && let Some(value) = args.get_mut(at + 1)
        {
            *value = host.to_string();
        }
        args
    }

    /// The run config that start arguments from [`serve_args`](Self::serve_args) serve.
    pub fn served_config(args: &[String]) -> Option<PathBuf> {
        let at = args.iter().position(|a| a == "--profile")?;
        args.get(at + 1).map(PathBuf::from)
    }

    /// Writes the Terminal's own files as `install.ps1` leaves them, empty,
    /// with `version` in `VERSION`; for tests.
    pub fn fake_install(&self, version: &str) {
        let root = self.current();
        for (file, text) in [
            (root.join("strata.py"), ""),
            (root.join("setup.py"), ""),
            (root.join("python").join("python.exe"), ""),
            (self.program(), "@echo off"),
        ] {
            std::fs::create_dir_all(file.parent().expect("a file in a folder")).unwrap();
            std::fs::write(file, text).unwrap();
        }
        std::fs::write(root.join("VERSION"), format!("{version}\n")).unwrap();
    }

    fn install_env(&self) -> (String, String) {
        (
            "STRATA_INSTALL_DIR".into(),
            self.install_dir().display().to_string(),
        )
    }
}

/// Reads what `strata update --output jsonl` printed: `Ok` when the engine
/// for this PC is in place, else Strata's reason. The answer is the last
/// line that is a JSON object and no event (`strata_cli/update_cmd.py`,
/// `run`).
pub fn engine_installed(output: &[String]) -> Result<(), String> {
    let answer =
        json_answer(output).ok_or("The Strata Terminal gave no answer about the engine")?;
    refused(&answer, "The engine update failed")?;
    let engine = answer["results"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["component"] == "engine"))
        .ok_or("The Strata Terminal's answer names no engine")?;
    part_problem(engine).map_or(Ok(()), Err)
}

/// The engine folder `strata components engine --output json` names
/// (`_components_engine` in `strata_cli/cli.py`), or why there is none: a
/// refusal, or `ok` false with Strata's notice (exit 5).
pub fn engine_root(output: &[String]) -> Result<PathBuf, String> {
    let answer =
        json_answer(output).ok_or("The Strata Terminal gave no answer about its engine folder")?;
    refused(&answer, "The Strata Terminal named no engine folder")?;
    if answer["ok"] != true {
        return Err(text(&answer["notice"])
            .unwrap_or("The Strata Terminal has no engine for this PC")
            .to_string());
    }
    text(&answer["engine"]["root"])
        .map(PathBuf::from)
        .ok_or_else(|| "The Strata Terminal's answer names no engine folder".into())
}

fn read_version(folder: &Path) -> Option<String> {
    first_line(&folder.join("VERSION"))
}

/// A one-line file's text, if it has any.
fn first_line(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string()).filter(|t| !t.is_empty())
}

/// `0.2.2`, `0.1.44.1` or `0.2.2-rc1` in an order to compare: a
/// pre-release comes before the same number without one.
fn version_key(text: &str) -> Option<([u64; 4], bool, String)> {
    let (base, pre) = match text.trim().split_once('-') {
        Some((base, pre)) => (base, Some(pre)),
        None => (text.trim(), None),
    };
    let parts: Vec<u64> = base
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut key = [0; 4];
    key[..parts.len()].copy_from_slice(&parts);
    Some((key, pre.is_none(), pre.unwrap_or_default().to_string()))
}

/// The version and build from `strata --version`, e.g. `0.2.2 (build
/// 0.1.44.1)`, without the Python it names.
pub fn version_text(output: &[String]) -> Option<String> {
    output.iter().find_map(|line| {
        let rest = line.trim().strip_prefix("strata ")?;
        let (version, rest) = rest.split_once(" (build ")?;
        let build = rest.split([';', ')']).next()?.trim();
        Some(format!("{version} (build {build})"))
    })
}

/// Strata's message when the answer is a refusal
/// (`{"ok": false, "error": {"code", "message"}}`).
fn refused(answer: &Value, fallback: &str) -> Result<(), String> {
    match answer.get("error") {
        Some(error) => Err(text(&error["message"]).unwrap_or(fallback).to_string()),
        None => Ok(()),
    }
}

/// Whether `output` holds an answer of Strata's, a refusal among them.
pub fn answered(output: &[String]) -> bool {
    json_answer(output).is_some()
}

/// The last line of `output` that is a JSON object and no event: what
/// `--output json` answers, and `--output jsonl` after its events.
fn json_answer(output: &[String]) -> Option<Value> {
    output.iter().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .filter(|v| v.is_object() && v.get("event").is_none())
    })
}

/// What `strata update --output jsonl` reports on its way, one line each
/// (`emit` in `strata_control/updater.py`).
#[derive(Debug, PartialEq)]
pub enum UpdateEvent {
    /// A new phase: reading the feed, a download, unpacking, the signature
    /// check, the test start, the switch, a warning.
    Phase(String),
    /// How far a download is (`_download`).
    Download {
        done: u64,
        total: u64,
        percent: u8,
        bytes_per_sec: Option<u64>,
    },
    /// An engine still serves and the update waits (`wait_until_idle`).
    Waiting(String),
}

pub fn update_event(line: &str) -> Option<UpdateEvent> {
    let event: Value = serde_json::from_str(line.trim()).ok()?;
    match text(&event["event"])? {
        "phase" => Some(UpdateEvent::Phase(text(&event["message"])?.to_string())),
        "progress" => Some(UpdateEvent::Download {
            done: event["done"].as_u64()?,
            total: event["total"].as_u64()?,
            percent: event["percent"].as_u64()?.min(100) as u8,
            bytes_per_sec: event["bytes_per_second"].as_u64(),
        }),
        "waiting" => Some(UpdateEvent::Waiting(text(&event["message"])?.to_string())),
        _ => None,
    }
}

/// Reads what `strata update --check --output json` printed: whether the
/// engine or the Terminal's files have something newer for this PC
/// (`check` in `strata_control/updater.py`).
pub fn update_available(output: &[String]) -> Result<bool, String> {
    let answer = json_answer(output).ok_or("The Strata Terminal gave no answer about updates")?;
    refused(&answer, "The update check failed")?;
    Ok(matches!(
        text(&answer["engine"]["status"]),
        Some("available" | "install")
    ) || text(&answer["control"]["status"]) == Some("available"))
}

/// Reads what `strata update --output json` printed: `Ok` when every part
/// is in place, else Strata's reasons (`run` in `strata_cli/update_cmd.py`).
pub fn updated(output: &[String]) -> Result<(), String> {
    let answer =
        json_answer(output).ok_or("The Strata Terminal gave no answer about the update")?;
    refused(&answer, "The update failed")?;
    let results = answer["results"]
        .as_array()
        .ok_or("The Strata Terminal's answer names no parts")?;
    // An engine version that failed its test start before, or was rolled
    // back from, is left alone without --force: nothing to do, as the
    // check says.
    let left_alone = |r: &&Value| {
        r["component"] == "engine"
            && matches!(text(&r["status"]), Some("failed-before" | "skipped"))
    };
    let problems: Vec<String> = results
        .iter()
        .filter(|r| !left_alone(r))
        .filter_map(part_problem)
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// What keeps one part of an update answer from being in place.
fn part_problem(result: &Value) -> Option<String> {
    let status = text(&result["status"]);
    let control = result["component"] == "control";
    match status {
        Some("updated" | "current") => None,
        Some("not-in-feed") if control => None,
        Some("failed") => Some(
            text(&result["error"])
                .unwrap_or("A part could not be installed")
                .to_string(),
        ),
        _ => {
            let mut why = text(&result["note"])
                .or(status)
                .unwrap_or("No engine for this PC")
                .to_string();
            let reasons: Vec<&str> = result["reasons"]
                .as_array()
                .map(|r| r.iter().filter_map(text).collect())
                .unwrap_or_default();
            if !reasons.is_empty() {
                why = format!("{why} ({})", reasons.join("; "));
            }
            Some(why)
        }
    }
}

/// The tuner's progress from its status file (`state` in `auto` of
/// `strata_cli/tune_commands.py`): Strata's message as the step, the share
/// of planned engine runs done, the time left it estimates and the
/// measurements so far (`speeds`, after `Tuner._say`).
#[derive(Debug, PartialEq)]
pub struct TuneProgress {
    pub step: String,
    pub percent: Option<u8>,
    pub eta_secs: Option<u64>,
    pub speeds: Vec<TuneSpeed>,
}

pub fn tune_progress(status: &str) -> Option<TuneProgress> {
    let state: Value = serde_json::from_str(status).ok()?;
    let step = text(&state["message"])?.to_string();
    // Before the first finished run there is nothing to count: a quick tune's
    // file can stay at that state to its end, which read as a stuck 0 %.
    let done = state["runs_done"].as_u64().filter(|d| *d > 0);
    let planned = state["runs_planned"].as_u64().filter(|p| *p > 0);
    let percent = done
        .zip(planned)
        .map(|(done, planned)| (done * 100 / planned).min(99) as u8);
    let speeds = state["speeds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(TuneSpeed {
                label: text(&s["label"])?.to_string(),
                tok_s: s["tok_s"].as_f64()?,
            })
        })
        .collect();
    Some(TuneProgress {
        step,
        percent,
        eta_secs: done.and(state["eta_s"].as_u64()),
        speeds,
    })
}

/// Reads what `strata tune auto --output json` printed: `{"ok": true,
/// "result": …}` with the tuner's result (`TuneResult` in
/// `strata_control/autotune.py`, after `finalize`), or Strata's reason.
pub fn tune_result(
    output: &[String],
    mode: TuneMode,
    usage: TuneUse,
    at: u64,
) -> Result<StrataTune, String> {
    let answer = json_answer(output).ok_or("The tuner gave no answer")?;
    refused(&answer, "The tuner failed")?;
    let result = &answer["result"];
    let code = text(&result["code"]).ok_or("The tuner's answer has no result")?;
    let values = &result["values"];
    let changed = settings_text(&result["settings"]);
    let preset = &result["preset"];
    Ok(StrataTune {
        at,
        mode,
        usage,
        code: code.to_string(),
        base_tps: values["base_tok_s"].as_f64(),
        tuned_tps: values["tuned_tok_s"].as_f64(),
        gain_pct: values["gain_pct"].as_f64(),
        changed,
        message: text(&result["message"]).map(String::from),
        preset: text(&preset["id"]).map(String::from),
        preset_name: text(&preset["name"]).map(String::from),
    })
}

/// What tuned `settings` (`args`, `env`, `per_request`) set: `--spec 3`,
/// a switch alone, `NAME=value`, `per request temperature 0.6`.
fn settings_text(settings: &Value) -> Vec<String> {
    let mut changed = Vec::new();
    for (flag, value) in settings["args"].as_object().into_iter().flatten() {
        match value {
            Value::Bool(false) | Value::Null => {}
            Value::Bool(true) => changed.push(flag.clone()),
            other => changed.push(format!("{flag} {}", value_text(other))),
        }
    }
    for (name, value) in settings["env"].as_object().into_iter().flatten() {
        changed.push(format!("{name}={}", value_text(value)));
    }
    for (name, value) in settings["per_request"].as_object().into_iter().flatten() {
        changed.push(format!("per request {name} {}", value_text(value)));
    }
    changed
}

/// Reads what `strata presets … --output json` printed: `Ok` unless Strata
/// refused (`_fail` in `strata_cli/tune_commands.py`).
pub fn preset_answer(output: &[String]) -> Result<(), String> {
    let answer = json_answer(output).ok_or("Strata gave no answer about the preset")?;
    refused(&answer, "Strata refused the preset change")
}

/// Reads what `strata presets status --output json` printed (`_status`
/// in `strata_cli/tune_commands.py`): the active preset with Strata's
/// verdict on it, else the newest one (Strata lists them newest first).
/// `last_tuned` is the preset the model's last tune saved: missing here, it
/// belongs to other choices. `None` without an answer or when Strata
/// refused.
pub fn read_presets(output: &[String], last_tuned: Option<&str>) -> Option<StrataPresets> {
    let answer = json_answer(output)?;
    refused(&answer, "").ok()?;
    let listed = list(&answer["presets"]);
    let active = &answer["active"];
    let latest = if active.is_object() {
        preset(active).map(|mut p| {
            p.state = match answer["state"].as_str() {
                Some("fresh") => Some(PresetState::Fresh),
                Some("soft") => Some(PresetState::Soft),
                Some("hard") => Some(PresetState::Hard),
                _ => None,
            };
            p.reasons = values(&answer["reasons"]);
            p
        })
    } else {
        listed.first().and_then(preset)
    };
    let other_context =
        last_tuned.is_some_and(|id| !listed.iter().any(|p| text(&p["id"]) == Some(id)));
    Some(StrataPresets {
        latest,
        other_context,
    })
}

/// A preset as `_summary` in `strata_cli/tune_commands.py` writes it.
fn preset(summary: &Value) -> Option<StrataPreset> {
    Some(StrataPreset {
        id: text(&summary["id"])?.to_string(),
        name: text(&summary["name"])?.to_string(),
        active: summary["active"].as_bool().unwrap_or(false),
        state: None,
        reasons: Vec::new(),
        gain_pct: summary["gain_pct"].as_f64(),
        settings: settings_text(&summary["settings"]),
        decode_tps: summary["decode_tok_s"].as_f64(),
        base_tps: summary["baseline_decode_tok_s"].as_f64(),
    })
}

/// Reads what [`Terminal::options`] printed for Terminal build `version`: `None`
/// without its answer. The engine's help counts only with a flag in it,
/// as in Strata's own read (`strata_cli/launch/params.py`).
pub fn read_options(output: &[String], version: &str) -> Option<StrataOptions> {
    let answer = json_answer(output)?;
    let tuner = &answer["tuner"];
    let engine_help = text(&answer["engine_help"])
        .map(str::trim)
        .filter(|h| h.contains("--"))
        .map(String::from);
    Some(StrataOptions {
        version: version.to_string(),
        tuner: tuner_options(tuner),
        never: never_rules(&tuner["never"]),
        tries: tuner_tries(&tuner["arms"]),
        engine_help,
        switches: speed_switches(&answer["switches"]),
        documented: documented(answer["rdna2"].as_bool() == Some(true)),
    })
}

fn speed_switches(switches: &Value) -> Vec<SpeedSwitch> {
    list(switches)
        .iter()
        .filter_map(|switch| {
            Some(SpeedSwitch {
                label: text(&switch["label"])?.to_string(),
                effect: text(&switch["effect"]).unwrap_or_default().to_string(),
                default_on: text(&switch["default"]) != Some("off"),
                env: switch["env"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(name, set)| Some((name.clone(), value(set)?)))
                    .collect(),
                models: values(&switch["models"]),
            })
        })
        .collect()
}

/// One engine environment variable Strata documents but lists in no table
/// (`docs/AMD_HIP.md` *RDNA2 (gfx1030)*, `docs/DETAILS.md`,
/// `docs/MESSAGE_BOUNDARY_CACHE.md`), as of Strata 0.2.7.
struct Documented {
    name: &'static str,
    value: Option<&'static str>,
    text: &'static str,
    changes_answers: bool,
    rdna2_only: bool,
    needs_args: &'static [&'static str],
    not_with_args: &'static [&'static str],
}

const DOCUMENTED: [Documented; 5] = [
    Documented {
        name: "STRATA_HIP_PROMPT_F16",
        value: Some("1"),
        text: "The prompt's 16-bit matrix products run FP16 in and out, which rocBLAS is tuned for \
               on RX 6000 cards: long prompts read about 2x faster (2x RX 6900 XT: 439-466 to \
               744-926 tok/s), decode unchanged. Values above 65504 overflow FP16.",
        changes_answers: true,
        rdna2_only: true,
        needs_args: &[],
        not_with_args: &[],
    },
    Documented {
        name: "STRATA_RDNA2_SGEMM",
        value: Some("1"),
        text: "Both inputs of the prompt's 16-bit matrix products are widened to FP32 and run as \
               SGEMM, with no FP16 limit (RX 6800 on Windows, 12K-token prompts: 312 to 452 \
               tok/s, decode unchanged). Slower than `STRATA_HIP_PROMPT_F16`; with both set the \
               FP16 route runs first.",
        changes_answers: true,
        rdna2_only: true,
        needs_args: &[],
        not_with_args: &[],
    },
    Documented {
        name: "STRATA_HC_PERSIST",
        value: Some("1"),
        text: "The verifier's hyper-connection read runs as one persistent kernel: 2.8 to 6.6% \
               less time per decode window (2x RX 6900 XT). Measured on Linux only.",
        changes_answers: true,
        rdna2_only: true,
        needs_args: &[],
        not_with_args: &[],
    },
    Documented {
        name: "STRATA_RESIDENT_HEADROOM_GIB",
        value: None,
        text: "The GiB of available RAM the resident copy of the experts leaves free (default \
               4). Lower it a little when the load lines say some experts are not kept in RAM; \
               raise it if the PC swaps.",
        changes_answers: false,
        rdna2_only: false,
        needs_args: &["--resident-experts", "--resident-budget-gib"],
        not_with_args: &[],
    },
    Documented {
        name: "STRATA_CACHE_MESSAGE_BOUNDARY",
        value: Some("1"),
        text: "Saves a checkpoint at the previous turn, so a coding tool that resends a history of \
               8K tokens or more with a changed last message does not read it again. Validated \
               by Strata only on NVIDIA; adds a little time to a fresh request.",
        changes_answers: false,
        rdna2_only: false,
        needs_args: &[],
        not_with_args: &["--layer-split"],
    },
];

/// The documented variables for this PC: those for RX 6000 cards only when
/// Strata's hardware detection found one.
fn documented(rdna2: bool) -> Vec<DocumentedVariable> {
    let strings = |args: &[&str]| args.iter().map(|a| a.to_string()).collect();
    DOCUMENTED
        .iter()
        .filter(|d| rdna2 || !d.rdna2_only)
        .map(|d| DocumentedVariable {
            name: d.name.into(),
            value: d.value.map(String::from),
            text: d.text.into(),
            changes_answers: d.changes_answers,
            needs_args: strings(d.needs_args),
            not_with_args: strings(d.not_with_args),
        })
        .collect()
}

fn tuner_options(tuner: &Value) -> Vec<TunerOption> {
    let bounds = tuner["bounds"].as_object();
    let answers = tuner["answer_of_flag"].as_object();
    let switches: Vec<&str> = list(&tuner["bool_flags"])
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    let arms = list(&tuner["arms"]);
    let mut flags: Vec<&str> = bounds
        .into_iter()
        .chain(answers)
        .flat_map(|o| o.keys().map(String::as_str))
        .chain(switches.iter().copied())
        .chain(
            arms.iter()
                .filter_map(|a| a["set_args"].as_object())
                .flat_map(|s| s.keys().map(String::as_str)),
        )
        .filter(|f| f.starts_with("--"))
        .collect();
    flags.sort_unstable();
    flags.dedup();
    flags
        .into_iter()
        .map(|flag| {
            let bound = bounds.and_then(|b| b.get(flag)).unwrap_or(&Value::Null);
            let takes_value = !switches.contains(&flag);
            TunerOption {
                flag: flag.to_string(),
                takes_value,
                min: value(&bound["min"]),
                max: value(&bound["max"]),
                choices: values(&bound["choices"]),
                answer: answers
                    .and_then(|a| a.get(flag))
                    .and_then(Value::as_str)
                    .unwrap_or("exact")
                    .to_string(),
            }
        })
        .collect()
}

fn never_rules(never: &Value) -> Vec<NeverRule> {
    list(never)
        .iter()
        .filter_map(|rule| {
            let (setting, env) = match (text(&rule["flag"]), text(&rule["env"])) {
                (Some(flag), _) => (flag, false),
                (None, Some(env)) => (env, true),
                (None, None) => return None,
            };
            let mut values = values(&rule["values"]);
            values.extend(value(&rule["value"]));
            Some(NeverRule {
                setting: setting.to_string(),
                env,
                values,
                test: text(&rule["test"]).map(String::from),
                limit: value(&rule["limit"]),
                when: self::values(&rule["when"]),
                families: self::values(&rule["families"]),
                reason: text(&rule["reason"]).unwrap_or_default().to_string(),
            })
        })
        .collect()
}

fn tuner_tries(arms: &Value) -> Vec<TunerTry> {
    list(arms)
        .iter()
        .filter_map(|arm| {
            let mut args = Vec::new();
            for (flag, set) in arm["set_args"].as_object().into_iter().flatten() {
                match set {
                    Value::Bool(false) | Value::Null => {}
                    Value::Bool(true) => args.push(flag.clone()),
                    other => args.extend([flag.clone(), value_text(other)]),
                }
            }
            Some(TunerTry {
                label: text(&arm["label"])?.to_string(),
                answer: text(&arm["answer"]).unwrap_or("exact").to_string(),
                args,
                env: arm["env"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, set)| (name.clone(), value(set)))
                    .collect(),
                families: values(&arm["families"]),
            })
        })
        .collect()
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn values(v: &Value) -> Vec<String> {
    list(v).iter().filter_map(value).collect()
}

/// A value Strata gave, `None` for none or `null`.
fn value(v: &Value) -> Option<String> {
    (!v.is_null()).then(|| value_text(v))
}

/// A value as Strata writes it on the command line.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Tokens/s from the standard benchmark's answer
/// (`strata bench run --output json`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BenchSpeeds {
    /// Median decode speed of the short requests.
    pub decode: f64,
    /// Median prompt speed of the 4K prompts, if there is one.
    pub prompt: Option<f64>,
}

/// Reads the standard benchmark's answer: the medians of its performance
/// phase (`summaries_of` in `strata_cli/bench_standard.py`).
pub fn bench_speeds(output: &[String]) -> Result<BenchSpeeds, String> {
    let answer = json_answer(output).ok_or("The benchmark gave no answer")?;
    refused(&answer, "The benchmark failed")?;
    let status = text(&answer["status"]).unwrap_or("unknown");
    if status != "complete" {
        return Err(format!("The benchmark did not complete (run {status})"));
    }
    let median = |case: &str, metric: &str| {
        answer["summaries"].as_array()?.iter().find_map(|row| {
            (row["phase"] == "performance" && row["case_id"] == case && row["metric"] == metric)
                .then(|| row["median"].as_f64())
                .flatten()
        })
    };
    let decode = median("short", "decode_tps").ok_or("The benchmark reported no tokens/s")?;
    Ok(BenchSpeeds {
        decode,
        prompt: median("prompt-4k", "prompt_tps"),
    })
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| l.to_string()).collect()
    }

    fn output(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    // The answers as `run` in strata_cli/update_cmd.py prints them (Strata
    // 011c9e6): `{"schema_version": 1, "ok": …, **run_update(…)}` with the
    // engine's result from `update_engine` in strata_control/updater.py.

    #[test]
    fn an_updated_or_current_engine_is_in_place() {
        for status in ["updated", "current"] {
            let out = lines(&[
                "note: something before",
                &format!(
                    r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "https://github.com/Niko1221/Strata/releases/latest/download/strata-feed.json", "sequence": 41, "channel": "stable", "key_id": "5d1bc23abbeffd0d"}}, "results": [{{"component": "engine", "status": "{status}", "installed_before": null, "changed": true}}], "exit_code": 0}}"#
                ),
            ]);
            assert_eq!(engine_installed(&out), Ok(()), "{status}");
        }
    }

    #[test]
    fn no_engine_for_this_pc_says_strata_s_note_and_reasons() {
        let out = lines(&[
            r#"{"schema_version": 1, "ok": true, "feed": {"source": "f", "sequence": 41, "channel": "stable", "key_id": "k"}, "results": [{"component": "engine", "status": "none-for-this-pc", "installed_before": null, "changed": false, "note": "The feed has no engine package for this PC.", "reasons": ["hip gfx1030: no package for this card"]}], "exit_code": 0}"#,
        ]);
        assert_eq!(
            engine_installed(&out),
            Err("The feed has no engine package for this PC. (hip gfx1030: no package for this card)".into())
        );
    }

    #[test]
    fn a_failed_install_or_an_error_says_why() {
        let failed = lines(&[
            r#"{"schema_version": 1, "ok": false, "feed": {"source": "f", "sequence": 41, "channel": "stable", "key_id": "k"}, "results": [{"component": "engine", "status": "failed", "changed": false, "error": "the test start failed", "exit_code": 6}], "exit_code": 6}"#,
        ]);
        assert_eq!(
            engine_installed(&failed),
            Err("the test start failed".into())
        );
        let error = lines(&[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "the feed's signature is not valid"}}"#,
        ]);
        assert_eq!(
            engine_installed(&error),
            Err("the feed's signature is not valid".into())
        );
        assert!(engine_installed(&lines(&["Traceback (most recent call last):"])).is_err());
    }

    /// What the options script printed with Strata 011c9e6: the tables of
    /// `strata_control/tune_knobs.py` as the script dumps them, and as the
    /// engine's help the text of `usage()` in `src/program/generate.cpp`,
    /// which the engine writes to stderr.
    const OPTIONS: &str = include_str!("../fixtures/strata-options.json");

    #[test]
    fn the_options_are_the_tuner_s_table_and_the_engine_s_help() {
        let out = lines(&["a warning before", OPTIONS.trim_end()]);
        let options = read_options(&out, "0.3.0").unwrap();
        assert_eq!(options.version, "0.3.0");

        let option = |flag: &str| {
            options
                .tuner
                .iter()
                .find(|o| o.flag == flag)
                .unwrap_or_else(|| panic!("{flag} missing"))
        };
        let spec = option("--spec");
        assert_eq!(spec.choices, ["3", "4"]);
        assert_eq!(spec.answer, "placement");
        assert!(spec.takes_value);
        let pcie = option("--pcie-frac");
        assert_eq!(
            (pcie.min.as_deref(), pcie.max.as_deref()),
            (Some("0.0"), Some("0.55"))
        );
        assert_eq!(
            option("--prefill").choices,
            ["auto", "auto:16384", "auto:32768", "2048", "4096"]
        );
        assert_eq!(option("--kv").answer, "quality");
        assert_eq!(option("--adapt-async").answer, "nonrepeatable");
        let switch = option("--kv-grow");
        assert!(!switch.takes_value);
        assert_eq!(switch.answer, "exact");
        let mut sorted = options.tuner.clone();
        sorted.sort_by(|a, b| a.flag.cmp(&b.flag));
        assert_eq!(sorted, options.tuner);

        assert_eq!(options.never.len(), 18);
        let first = &options.never[0];
        assert_eq!(first.setting, "STRATA_SH_STREAM");
        assert!(first.env);
        assert_eq!(first.when, ["gfx12"]);
        assert_eq!(first.reason, "it halves decode speed on this graphics card");
        assert_eq!(options.never[1].values, ["all"]);
        let affinity = &options.never[4];
        assert_eq!(affinity.setting, "--pool-affinity");
        assert!(!affinity.env);
        assert_eq!(affinity.values, ["auto", "p-cores"]);
        let spec = options
            .never
            .iter()
            .find(|r| r.families == ["flash-next"])
            .unwrap();
        assert_eq!(
            (
                spec.setting.as_str(),
                spec.test.as_deref(),
                spec.limit.as_deref()
            ),
            ("--spec", Some("at_most"), Some("2"))
        );

        assert_eq!(options.tries.len(), 16);
        let rebalance = &options.tries[3];
        assert_eq!(rebalance.label, "Re-balance the expert cache faster");
        assert_eq!(rebalance.answer, "placement");
        assert_eq!(
            rebalance.args,
            [
                "--adapt-every",
                "1",
                "--adapt-swaps",
                "80",
                "--adapt-decay",
                "0.97"
            ]
        );
        assert_eq!(rebalance.families, ["flash-next"]);
        assert_eq!(options.tries[6].args, ["--trim-stage-weights"]);
        assert!(options.tries[8].args.is_empty());
        assert!(options.tries[8].env.is_empty());
        let pin = &options.tries[12];
        assert!(pin.args.is_empty());
        assert_eq!(
            pin.env,
            [("STRATA_PIN_BY_NEED".to_string(), Some("1".to_string()))].into()
        );
        assert_eq!(options.tries[9].families, ["qwen36"]);

        let help = options.engine_help.unwrap();
        assert!(help.starts_with("strata generate --pack DIR"), "{help}");
        assert!(help.contains("  --kv-grow "), "{help}");
        assert!(help.contains("54.4% hits"), "{help}");
    }

    #[test]
    fn options_without_a_table_or_an_engine_keep_what_was_read() {
        let out = lines(&[
            r#"{"tuner_error": "No module named 'strata_control.tune_knobs'", "engine_help": "\n'strata.exe' is not recognized"}"#,
        ]);
        let options = read_options(&out, "0.2.2").unwrap();
        assert!(options.tuner.is_empty() && options.never.is_empty());
        assert_eq!(options.engine_help, None);
        assert_eq!(
            read_options(&lines(&["Traceback (most recent call last):"]), "0.2.2"),
            None
        );
    }

    /// The options script's output with Strata 0.2.7's speed switches
    /// (`strata_control/speed_switches.py`) on a PC with an RX 6000 card,
    /// warnings of Strata's setup module first. Run by hand from another
    /// folder, so the tuner's table was not found (`tuner_error`).
    const OPTIONS_027: &str = include_str!("../fixtures/strata-options-0.2.7.json");

    #[test]
    fn speed_switches_are_read_with_the_models_they_apply_to() {
        let options = read_options(&output(OPTIONS_027), "0.2.7").unwrap();
        assert_eq!(options.switches.len(), 5);
        let long = &options.switches[0];
        assert_eq!(long.label, "Faster long prompts");
        assert!(long.effect.starts_with("Reads very long prompts faster"));
        assert!(!long.default_on);
        assert_eq!(long.env, [("STRATA_KVA".into(), "1".into())].into());
        assert_eq!(long.models, ["coder-iq1_m", "iq3_s", "iq3_xxs", "q2_0"]);
        let nvidia = &options.switches[1];
        assert!(nvidia.default_on);
        assert!(nvidia.models.is_empty());
        assert_eq!(options.switches[3].env.len(), 7);
        assert!(options.engine_help.is_some());
    }

    #[test]
    fn an_rx_6000_card_gets_its_documented_variables() {
        let options = read_options(&output(OPTIONS_027), "0.2.7").unwrap();
        let names: Vec<&str> = options.documented.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "STRATA_HIP_PROMPT_F16",
                "STRATA_RDNA2_SGEMM",
                "STRATA_HC_PERSIST",
                "STRATA_RESIDENT_HEADROOM_GIB",
                "STRATA_CACHE_MESSAGE_BOUNDARY"
            ]
        );
        let f16 = &options.documented[0];
        assert_eq!(f16.value.as_deref(), Some("1"));
        assert!(f16.changes_answers);
        assert!(f16.text.contains("2x faster"), "{}", f16.text);
        let headroom = &options.documented[3];
        assert_eq!(headroom.value, None);
        assert!(!headroom.changes_answers);
        assert_eq!(
            headroom.needs_args,
            ["--resident-experts", "--resident-budget-gib"]
        );
        assert_eq!(options.documented[4].not_with_args, ["--layer-split"]);
    }

    #[test]
    fn without_an_rx_6000_card_its_variables_are_left_out() {
        let other = OPTIONS_027.replace(r#""rdna2": true"#, r#""rdna2": false"#);
        let undetected = OPTIONS_027.replace(
            r#""rdna2": true"#,
            r#""hardware_error": "No module named 'wmi'""#,
        );
        assert_ne!(other, OPTIONS_027);
        assert_ne!(undetected, OPTIONS_027);
        for out in [other, undetected] {
            let options = read_options(&output(&out), "0.2.7").unwrap();
            let names: Vec<&str> = options.documented.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    "STRATA_RESIDENT_HEADROOM_GIB",
                    "STRATA_CACHE_MESSAGE_BOUNDARY"
                ]
            );
        }
    }

    #[test]
    fn a_strata_without_speed_switches_keeps_the_rest() {
        let options = read_options(&lines(&[OPTIONS.trim_end()]), "0.2.2").unwrap();
        assert!(options.switches.is_empty());
        assert!(!options.tuner.is_empty() && options.engine_help.is_some());
    }

    #[test]
    fn the_options_script_gets_the_run_config() {
        let terminal = Terminal::new("C:/Users/me/AppData/Local/Strata");
        let config = Path::new("C:/Users/me/AppData/Local/Strata/install/strata-iq3_xxs.json");
        let spec = terminal.options(Some(config));
        assert_eq!(
            spec.program,
            terminal.root().join("python").join("python.exe")
        );
        assert_eq!(spec.args[0], "-c");
        assert_eq!(spec.args[2], config.display().to_string());
        assert_eq!(terminal.options(None).args.len(), 2);
    }

    /// A finished tune as `auto` in strata_cli/tune_commands.py prints it
    /// (Strata 011c9e6): `{"schema_version": 1, "ok": true, "result": out}`
    /// with `TuneResult.to_dict()` from strata_control/autotune.py, the saved
    /// preset and `finalize`'s `code` and `values`.
    const TUNED: &str = include_str!("../fixtures/strata-tune.json");
    /// `strata presets status --output json` with an active preset Strata
    /// offers to re-check, in the shape of `_status` and `_summary` in
    /// `strata_cli/tune_commands.py`; the reason is one of `staleness` in
    /// `strata_control/presets.py`.
    const PRESETS: &str = include_str!("../fixtures/strata-presets-status.json");
    const ACTIVE: &str = "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6";

    #[test]
    fn the_active_preset_comes_with_strata_s_verdict() {
        let read = read_presets(&lines(&[PRESETS.trim_end()]), Some(ACTIVE)).unwrap();
        let latest = read.latest.unwrap();
        assert_eq!(latest.id, ACTIVE);
        assert_eq!(latest.name, "IQ3_XXS 64K – Full speed");
        assert!(latest.active);
        assert_eq!(latest.state, Some(PresetState::Soft));
        assert_eq!(latest.reasons, ["the graphics driver changed"]);
        assert_eq!(latest.gain_pct, Some(12.4));
        assert_eq!(latest.settings, ["--kv-resident 65536", "--kv-grow"]);
        assert_eq!(
            (latest.decode_tps, latest.base_tps),
            (Some(35.1), Some(31.2))
        );
        assert!(!read.other_context);
    }

    #[test]
    fn without_an_active_preset_the_newest_is_the_latest() {
        let mut answer: Value = serde_json::from_str(PRESETS).unwrap();
        answer["active"] = Value::Null;
        answer["state"] = Value::Null;
        answer["reasons"] = serde_json::json!([]);
        for p in answer["presets"].as_array_mut().unwrap() {
            p["active"] = false.into();
        }
        let read = read_presets(&[answer.to_string()], Some(ACTIVE)).unwrap();
        let latest = read.latest.unwrap();
        assert_eq!((latest.id.as_str(), latest.active), (ACTIVE, false));
        assert_eq!((latest.state, latest.reasons.len()), (None, 0));
    }

    #[test]
    fn a_tune_for_other_choices_means_none_for_this_context() {
        let mut answer: Value = serde_json::from_str(PRESETS).unwrap();
        answer["active"] = Value::Null;
        answer["presets"] = serde_json::json!([]);
        let read = read_presets(&[answer.to_string()], Some(ACTIVE)).unwrap();
        assert_eq!(read.latest, None);
        assert!(read.other_context);
        assert!(
            !read_presets(&[answer.to_string()], None)
                .unwrap()
                .other_context
        );
    }

    #[test]
    fn a_refused_status_reads_as_none() {
        let out = lines(&[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 2, "message": "no such profile"}}"#,
        ]);
        assert_eq!(read_presets(&out, None), None);
        assert_eq!(read_presets(&lines(&["Traceback"]), None), None);
    }

    #[test]
    fn a_faster_tune_gives_its_speeds_changes_and_preset() {
        let out = lines(&["Tuning qwen3.8 (quick)", TUNED.trim_end()]);
        let tune = tune_result(&out, TuneMode::Quick, TuneUse::Coding, 1_792_000_000).unwrap();
        assert_eq!(tune.code, "faster");
        assert_eq!((tune.base_tps, tune.tuned_tps), (Some(31.2), Some(35.1)));
        assert_eq!(tune.gain_pct, Some(12.4));
        assert_eq!(tune.changed, ["--kv-resident 65536", "--kv-grow"]);
        assert_eq!(
            tune.preset.as_deref(),
            Some("3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6")
        );
        assert_eq!(tune.preset_name.as_deref(), Some("IQ3_XXS 64K coding"));
        assert_eq!(tune.message, None);
        assert_eq!(
            (tune.at, tune.mode, tune.usage),
            (1_792_000_000, TuneMode::Quick, TuneUse::Coding)
        );
    }

    #[test]
    fn a_tune_without_gain_or_with_an_error_says_so() {
        // `default_confirmed` with Strata's sentence; env and per-request
        // settings as `settings` holds them.
        let same = lines(&[
            r#"{"schema_version": 1, "ok": true, "result": {"outcome": "inconclusive", "code": "busy_pc", "reason_code": "noisy", "values": {"gain_pct": 0.0, "base_tok_s": 30.9, "tuned_tok_s": null}, "message": "Your PC was too busy to compare settings. Nothing was changed.", "settings": {"args": {}, "env": {"STRATA_SELECT_SIMT": "1"}, "per_request": {"temperature": 0.6}}, "preset": null}}"#,
        ]);
        let tune = tune_result(&same, TuneMode::Thorough, TuneUse::Chat, 1).unwrap();
        assert_eq!(tune.code, "busy_pc");
        assert_eq!(tune.tuned_tps, None);
        assert_eq!(
            tune.changed,
            ["STRATA_SELECT_SIMT=1", "per request temperature 0.6"]
        );
        assert_eq!(tune.preset, None);
        assert_eq!(
            tune.message.as_deref(),
            Some("Your PC was too busy to compare settings. Nothing was changed.")
        );
        let refused = lines(&[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "no engine was found at strata.exe"}}"#,
        ]);
        assert_eq!(
            tune_result(&refused, TuneMode::Quick, TuneUse::Coding, 1),
            Err("no engine was found at strata.exe".into())
        );
        assert!(tune_result(&lines(&["Traceback"]), TuneMode::Quick, TuneUse::Coding, 1).is_err());
    }

    #[test]
    fn the_status_file_gives_step_share_time_left_and_speeds() {
        // As a thorough tune on the PC wrote it.
        let status = r#"{"stage": "rounds", "message": "Trying: Run the control thread on the last CPU core (round 1 of 3)", "state": "running", "elapsed_s": 631, "runs_done": 2, "runs_planned": 23, "eta_s": 6569, "speeds": [{"label": "base", "tok_s": 171.4}, {"label": "Keep more of the conversation in graphics memory", "tok_s": 172.2}], "minutes_left": 109, "text": "Finding the fastest settings for your PC… about 109 min left", "arm": "host_core_last"}"#;
        assert_eq!(
            tune_progress(status),
            Some(TuneProgress {
                step: "Trying: Run the control thread on the last CPU core (round 1 of 3)".into(),
                percent: Some(8),
                eta_secs: Some(6569),
                speeds: vec![
                    TuneSpeed {
                        label: "base".into(),
                        tok_s: 171.4
                    },
                    TuneSpeed {
                        label: "Keep more of the conversation in graphics memory".into(),
                        tok_s: 172.2
                    },
                ],
            })
        );
        // The first state, before any run is planned.
        let start = r#"{"stage": "start", "message": "Starting", "state": "running"}"#;
        assert_eq!(
            tune_progress(start),
            Some(TuneProgress {
                step: "Starting".into(),
                percent: None,
                eta_secs: None,
                speeds: Vec::new(),
            })
        );
        // Nothing measured yet, as a quick tune's file stayed to its end on
        // the PC: the step, but no share and no estimate.
        let measuring = r#"{"stage": "base", "message": "Measuring your current settings", "state": "running", "elapsed_s": 0, "runs_done": 0, "runs_planned": 6, "eta_s": 900, "speeds": [], "minutes_left": 15, "text": "Finding the fastest settings for your PC… about 15 min left"}"#;
        assert_eq!(
            tune_progress(measuring),
            Some(TuneProgress {
                step: "Measuring your current settings".into(),
                percent: None,
                eta_secs: None,
                speeds: Vec::new(),
            })
        );
        assert_eq!(tune_progress("{\"stage\": "), None);
    }

    #[test]
    fn update_events_give_phases_downloads_and_waits() {
        // As `emit` in `strata_control/updater.py` writes them, with the
        // `schema_version` that `--output jsonl` puts first.
        assert_eq!(
            update_event(
                r#"{"schema_version": 1, "event": "phase", "phase": "download", "message": "Downloading engine 0.1.45 windows-x86_64-cuda (412 MiB)", "component": "engine"}"#
            ),
            Some(UpdateEvent::Phase(
                "Downloading engine 0.1.45 windows-x86_64-cuda (412 MiB)".into()
            ))
        );
        assert_eq!(
            update_event(
                r#"{"schema_version": 1, "event": "progress", "phase": "download", "component": "engine", "label": "engine 0.1.45 windows-x86_64-cuda", "done": 104857600, "total": 432013312, "percent": 24, "bytes_per_second": 20971520}"#
            ),
            Some(UpdateEvent::Download {
                done: 104857600,
                total: 432013312,
                percent: 24,
                bytes_per_sec: Some(20971520),
            })
        );
        assert_eq!(
            update_event(
                r#"{"schema_version": 1, "event": "waiting", "message": "An engine is serving; waiting for it to stop before updating.", "serving": [{"profile": "a running Strata server", "state": "serving"}]}"#
            ),
            Some(UpdateEvent::Waiting(
                "An engine is serving; waiting for it to stop before updating.".into()
            ))
        );
        // The answer and plain lines are no events.
        assert_eq!(
            update_event(r#"{"schema_version": 1, "ok": true, "results": []}"#),
            None
        );
        assert_eq!(update_event("Traceback (most recent call last):"), None);
    }

    #[test]
    fn the_update_answer_comes_after_its_events() {
        let output = lines(&[
            r#"{"schema_version": 1, "event": "phase", "phase": "feed", "message": "Reading the signed update feed"}"#,
            r#"{"schema_version": 1, "ok": true, "exit_code": 0, "results": [{"component": "engine", "status": "updated"}]}"#,
        ]);
        assert_eq!(engine_installed(&output), Ok(()));
        // Events alone are no answer.
        assert_eq!(
            engine_installed(&output[..1]),
            Err("The Strata Terminal gave no answer about the engine".into())
        );
    }

    #[test]
    fn the_tune_command_names_the_run_config_and_the_choices() {
        let terminal = Terminal::new("C:/Users/me/AppData/Local/Strata");
        let config = Path::new("C:/Users/me/AppData/Local/Strata/install/strata-iq3_xxs.json");
        let status = Path::new("C:/ProgramData/strata-tune-status.json");
        let spec = terminal.tune(config, TuneMode::Thorough, TuneUse::Chat, status);
        assert_eq!(spec.program, terminal.program());
        assert_eq!(
            spec.args,
            [
                "tune",
                "auto",
                &config.display().to_string(),
                "--goal",
                "speed",
                "--mode",
                "thorough",
                "--use",
                "chat",
                "--status-file",
                &status.display().to_string(),
                "--output",
                "json",
            ]
        );
        assert_eq!(
            terminal.undo_preset("3f2a9c0e").args,
            ["presets", "undo", "3f2a9c0e", "--output", "json"]
        );
        assert_eq!(
            preset_answer(&lines(&[
                r#"{"schema_version": 1, "ok": true, "active": null}"#
            ])),
            Ok(())
        );
        assert_eq!(
            preset_answer(&lines(&[
                r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "there is nothing to undo"}}"#
            ])),
            Err("there is nothing to undo".into())
        );
    }

    #[test]
    fn the_engine_folder_comes_from_strata_components_engine() {
        // `_components_engine` prints `engine_component.status` with
        // `schema_version` and `ok` (Strata 4b9e05a).
        let found = r#"{"schema_version": 1, "ok": true, "components_dir": "C:\\Users\\me\\AppData\\Local\\Strata\\components", "platform": "windows-x86_64", "backend": "hip", "backend_source": "requested", "installed": [], "problems": [], "engine": {"id": "strata-engine", "version": "0.1.50", "platform": "windows-x86_64", "backend": "hip", "entrypoint": "strata.exe", "exe": "C:\\Users\\me\\AppData\\Local\\Strata\\components\\engine\\0.1.50-windows-x86_64-hip-rdna2\\strata.exe", "root": "C:\\Users\\me\\AppData\\Local\\Strata\\components\\engine\\0.1.50-windows-x86_64-hip-rdna2", "min_runtime": null, "archs": ["gfx1030"], "min_driver": {}, "build_id": "b", "variant": "rdna2", "signed": true, "file_count": 40, "bytes": 1}, "usable": true, "notice": null, "acquires": false, "pinned": null, "previous": null, "update_notice": null}"#;
        assert_eq!(
            engine_root(&lines(&[found])),
            Ok(PathBuf::from(
                r"C:\Users\me\AppData\Local\Strata\components\engine\0.1.50-windows-x86_64-hip-rdna2"
            ))
        );
        let none = r#"{"schema_version": 1, "ok": false, "engine": null, "usable": false, "notice": "no engine for this PC is installed"}"#;
        assert_eq!(
            engine_root(&lines(&[none])),
            Err("no engine for this PC is installed".into())
        );
        let refused = r#"{"schema_version": 1, "ok": false, "error": {"code": 2, "message": "unknown backend"}}"#;
        assert_eq!(
            engine_root(&lines(&[refused])),
            Err("unknown backend".into())
        );
        assert!(engine_root(&lines(&["Traceback (most recent call last):"])).is_err());
    }

    #[test]
    fn the_served_config_comes_back_from_the_start_arguments() {
        let config = Path::new("C:/Users/me/AppData/Local/Strata/install/strata-iq3_xxs.json");
        let args = Terminal::serve_args(config, "0.0.0.0", "8080");
        assert_eq!(args[0], "serve");
        assert_eq!(Terminal::served_config(&args).as_deref(), Some(config));
        assert_eq!(Terminal::served_config(&["--native".into()]), None);
    }

    #[test]
    fn the_build_is_the_version_file_s_line() {
        let home = std::env::temp_dir().join(format!("terminal-version-{}", std::process::id()));
        let terminal = Terminal::new(&home);
        assert_eq!(terminal.build(), None);
        terminal.fake_install("0.2.2");
        assert!(terminal.is_installed());
        assert_eq!(terminal.build().as_deref(), Some("0.2.2"));
        std::fs::write(terminal.root().join("VERSION"), " \n").unwrap();
        assert_eq!(terminal.build(), None);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_version_line_gives_version_and_build() {
        // `_VersionAction` in strata_cli/cli.py.
        let out = lines(&[
            r"strata 0.2.2 (build 0.1.44.1; python 3.12.15 at C:\Users\me\AppData\Local\Strata\terminal\current\python\python.exe)",
        ]);
        assert_eq!(
            version_text(&out).as_deref(),
            Some("0.2.2 (build 0.1.44.1)")
        );
        assert_eq!(version_text(&lines(&["usage: strata"])), None);
    }

    // `check` in strata_control/updater.py, printed by `run` in
    // strata_cli/update_cmd.py as `{"schema_version": 1, "ok": true, **report}`.
    fn check_answer(engine: &str, control: &str) -> Vec<String> {
        lines(&[&format!(
            r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "https://github.com/Niko1221/Strata/releases/latest/download/strata-feed.json", "sequence": 42, "channel": "stable", "key_id": "5d1bc23abbeffd0d", "generated_at": "2026-10-21T09:00:00Z", "trusted_by": "built-in keys"}}, "engine": {{"installed": {{"version": "0.1.44.1", "platform": "windows-x64", "backend": "hip", "variant": "gfx1030", "build_id": "b1", "folder": "strata-engine-0.1.44.1-hip", "signed": true}}, "offered": null, "reasons": [], "status": "{engine}", "download_bytes": 0, "previous": null, "pinned": null, "problem": null}}, "control": {{"installed": "0.1.44.1", "offered": "0.1.45", "status": "{control}", "download_bytes": 81234567}}, "app": {{"installed": null, "offered": null, "status": "not-in-feed", "notes": "", "download_bytes": 0}}, "serving": [], "hardware": {{"platform": "windows-x64", "backend": "hip"}}}}"#
        )])
    }

    #[test]
    fn an_update_is_available_when_the_engine_or_the_files_are_newer() {
        assert_eq!(
            update_available(&check_answer("current", "current")),
            Ok(false)
        );
        assert_eq!(
            update_available(&check_answer("available", "current")),
            Ok(true)
        );
        assert_eq!(
            update_available(&check_answer("current", "available")),
            Ok(true)
        );
        assert_eq!(
            update_available(&check_answer("failed-before", "not-in-feed")),
            Ok(false)
        );
        let error = lines(&[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 6, "message": "the feed could not be read: timed out"}}"#,
        ]);
        assert_eq!(
            update_available(&error),
            Err("the feed could not be read: timed out".into())
        );
    }

    #[test]
    fn an_update_is_done_when_every_part_is_in_place() {
        // `run_update` in strata_control/updater.py: one result per part.
        let answer = |control: &str| {
            lines(&[&format!(
                r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}}, "results": [{{"component": "engine", "status": "current", "installed_before": {{"version": "0.1.44.1"}}, "changed": false}}, {control}], "exit_code": 0}}"#
            )])
        };
        let updated_control = r#"{"component": "control", "status": "updated", "changed": true, "installed_before": "0.1.44.1", "installed_after": "0.1.45", "note": "the Terminal hands over to it from the next command; the app uses it from its next start"}"#;
        assert_eq!(updated(&answer(updated_control)), Ok(()));
        let not_in_feed = r#"{"component": "control", "status": "not-in-feed", "changed": false, "installed_before": "0.1.44.1"}"#;
        assert_eq!(updated(&answer(not_in_feed)), Ok(()));
        let engine_left = lines(&[
            r#"{"schema_version": 1, "ok": true, "feed": {"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}, "results": [{"component": "engine", "status": "failed-before", "installed_before": {"version": "0.1.44.1"}, "changed": false, "note": "0.1.45 failed its test start on 2026-10-21; run with --force to try it again"}, {"component": "control", "status": "updated", "changed": true}], "exit_code": 0}"#,
        ]);
        assert_eq!(updated(&engine_left), Ok(()));
        let failed = r#"{"component": "control", "status": "failed", "changed": false, "installed_before": "0.1.44.1", "error": "Control 0.1.45: --version said '' (exit 1)"}"#;
        assert_eq!(
            updated(&answer(failed)),
            Err("Control 0.1.45: --version said '' (exit 1)".into())
        );
        let busy = lines(&[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 7, "message": "an engine is serving (strata-iq3_xxs)"}}"#,
        ]);
        assert_eq!(
            updated(&busy),
            Err("an engine is serving (strata-iq3_xxs)".into())
        );
    }

    #[test]
    fn the_benchmark_speeds_are_the_performance_medians() {
        // `_bench_standard` in strata_cli/cli.py with the rows of
        // `summaries_of` in strata_cli/bench_standard.py.
        let row = |case: &str, metric: &str, median: &str| {
            format!(
                r#"{{"phase": "performance", "case_id": "{case}", "concurrency": 1, "metric": "{metric}", "unit": "tokens/s", "median": {median}, "min": null, "max": null, "n": 3, "included_request_ids": [], "excluded": []}}"#
            )
        };
        let answer = |status: &str, rows: &[String]| {
            lines(&[&format!(
                r#"{{"schema_version": 1, "ok": {}, "path": "C:\\bench\\results.json", "status": "{status}", "quality": {{"instruction": {{"passed": 4, "total": 4}}}}, "summaries": [{}]}}"#,
                status == "complete",
                rows.join(", ")
            )])
        };
        let full = answer(
            "complete",
            &[
                row("short", "prompt_tps", "210.4"),
                row("short", "decode_tps", "38.5"),
                row("prompt-4k", "prompt_tps", "512.3"),
                row("prompt-4k", "decode_tps", "36.1"),
            ],
        );
        assert_eq!(
            bench_speeds(&full),
            Ok(BenchSpeeds {
                decode: 38.5,
                prompt: Some(512.3)
            })
        );
        let short_only = answer("complete", &[row("short", "decode_tps", "38.5")]);
        assert_eq!(bench_speeds(&short_only).unwrap().prompt, None);
        let nothing = answer("complete", &[row("short", "decode_tps", "null")]);
        assert_eq!(
            bench_speeds(&nothing),
            Err("The benchmark reported no tokens/s".into())
        );
        let partial = answer("partial", &[row("short", "decode_tps", "38.5")]);
        assert_eq!(
            bench_speeds(&partial),
            Err("The benchmark did not complete (run partial)".into())
        );
    }

    #[test]
    fn newer_files_from_an_update_run_once_they_are_whole() {
        let home = std::env::temp_dir().join(format!("terminal-control-{}", std::process::id()));
        let terminal = Terminal::new(&home);
        terminal.fake_install("0.1.44.1");
        let current = terminal.current();
        assert_eq!(terminal.root(), current);
        let control = home.join("components").join("control");
        let newer = control.join("0.1.45");
        std::fs::create_dir_all(newer.join("strata_control")).unwrap();
        std::fs::write(newer.join("VERSION"), "0.1.45\n").unwrap();
        std::fs::write(
            control.join("active.json"),
            r#"{"version": "0.1.45", "folder": "0.1.45", "sha256": "ab"}"#,
        )
        .unwrap();
        assert_eq!(terminal.root(), current, "no interpreter yet");
        std::fs::create_dir_all(newer.join("python")).unwrap();
        std::fs::write(newer.join("python").join("python.exe"), "").unwrap();
        std::fs::write(newer.join("strata.py"), "").unwrap();
        assert_eq!(terminal.root(), newer);
        assert_eq!(terminal.build().as_deref(), Some("0.1.45"));
        assert_eq!(terminal.program(), current.join("bin").join("strata.cmd"));
        std::fs::write(newer.join("VERSION"), "0.1.44.1\n").unwrap();
        assert_eq!(terminal.root(), current, "never an equal one");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn versions_compare_by_number_and_a_pre_release_comes_first() {
        assert!(version_key("0.1.45") > version_key("0.1.44.1"));
        assert!(version_key("0.2.2") > version_key("0.2.2-rc1"));
        assert_eq!(version_key("unknown"), None);
    }

    #[test]
    fn the_install_script_gets_its_folder_and_a_quoted_address() {
        let terminal = Terminal::new("C:/Users/me/AppData/Local/Strata");
        let spec = terminal.install("https://example.org/it's/install.ps1");
        assert_eq!(
            spec.args.last().unwrap(),
            "irm 'https://example.org/it''s/install.ps1' | iex"
        );
        assert_eq!(
            spec.env,
            [(
                "STRATA_DATA_DIR".to_string(),
                "C:/Users/me/AppData/Local/Strata".to_string()
            )]
        );
    }
}
