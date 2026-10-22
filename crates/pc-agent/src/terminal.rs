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
use strata_pc_protocol::{NeverRule, StrataOptions, TunerOption, TunerTry};

use crate::command::CommandSpec;

/// The install script's folder for the Terminal itself, a link to the
/// version installed last.
const CURRENT: [&str; 2] = ["terminal", "current"];
/// The Strata folder with the run configs, beside the engines, where
/// Strata's desktop app keeps them too.
const INSTALL: &str = "install";
/// The `strata` command in the Terminal's own files.
const PROGRAM: [&str; 2] = ["bin", "strata.cmd"];
/// Prints the tuner's option table (`strata_control/tune_knobs.py`) and,
/// for the run config named as its argument, what its engine prints for
/// `--help`, run the way Strata's own tuner runs it
/// (`strata_control/tuning.py`). The Terminal has no command for either.
const OPTIONS_SCRIPT: &str = r#"import json, subprocess, sys
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
                  "families": plain(a.families)} for a in k.ARMS]}
except Exception as e:
    out["tuner_error"] = str(e)
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

/// The Strata Terminal in its folder (`strata_home`).
#[derive(Clone, Debug)]
pub struct Terminal {
    home: PathBuf,
}

impl Terminal {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn of(cfg: &crate::Config) -> Self {
        Self::new(&cfg.strata_home)
    }

    /// The Terminal's own files: `strata.py`, `setup.py`, `serve`, `data`.
    pub fn root(&self) -> PathBuf {
        CURRENT
            .iter()
            .fold(self.home.clone(), |p, part| p.join(part))
    }

    /// The `strata` command.
    pub fn program(&self) -> PathBuf {
        Self::program_in(&self.root())
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

    /// The Terminal's version as its `VERSION` file says, which is what
    /// `strata --version` prints as the build.
    pub fn version(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.root().join("VERSION")).ok()?;
        Some(text.trim().to_string()).filter(|v| !v.is_empty())
    }

    /// What makes up a running Strata: every program from the Terminal's
    /// folder (its Python) and the engines beside it.
    pub fn processes(&self) -> Vec<String> {
        vec![crate::platform::folder_entry(&self.home)]
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

    /// The engine for this PC from the signed feed; the Terminal itself
    /// stays as it is. Read the output with [`engine_installed`].
    pub fn install_engine(&self) -> CommandSpec {
        self.command(&["update", "--no-control", "--output", "json"])
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

    /// The run config that start arguments from [`serve_args`](Self::serve_args) serve.
    pub fn served_config(args: &[String]) -> Option<PathBuf> {
        let at = args.iter().position(|a| a == "--profile")?;
        args.get(at + 1).map(PathBuf::from)
    }

    /// Writes the Terminal's own files as `install.ps1` leaves them, empty,
    /// with `version` in `VERSION`; for tests.
    pub fn fake_install(&self, version: &str) {
        let root = self.root();
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

/// Reads what `strata update --output json` printed: `Ok` when the engine
/// for this PC is in place, else Strata's reason. The answer is the last
/// line that is a JSON object (`strata_cli/update_cmd.py`, `run`).
pub fn engine_installed(output: &[String]) -> Result<(), String> {
    let answer = output
        .iter()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<Value>(line.trim())
                .ok()
                .filter(Value::is_object)
        })
        .ok_or("The Strata Terminal gave no answer about the engine")?;
    if let Some(error) = answer.get("error") {
        return Err(text(&error["message"])
            .unwrap_or("The engine update failed")
            .to_string());
    }
    let engine = answer["results"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["component"] == "engine"))
        .ok_or("The Strata Terminal's answer names no engine")?;
    match text(&engine["status"]) {
        Some("updated" | "current") => Ok(()),
        Some("failed") => Err(text(&engine["error"])
            .unwrap_or("The engine could not be installed")
            .to_string()),
        status => {
            let mut why = text(&engine["note"])
                .or(status)
                .unwrap_or("No engine for this PC")
                .to_string();
            let reasons: Vec<&str> = engine["reasons"]
                .as_array()
                .map(|r| r.iter().filter_map(text).collect())
                .unwrap_or_default();
            if !reasons.is_empty() {
                why = format!("{why} ({})", reasons.join("; "));
            }
            Err(why)
        }
    }
}

/// Reads what [`Terminal::options`] printed for Terminal `version`: `None`
/// without its answer. The engine's help counts only with a flag in it,
/// as in Strata's own read (`strata_cli/launch/params.py`).
pub fn read_options(output: &[String], version: &str) -> Option<StrataOptions> {
    let answer = output.iter().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .filter(Value::is_object)
    })?;
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
    })
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

fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| l.to_string()).collect()
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

    #[test]
    fn the_served_config_comes_back_from_the_start_arguments() {
        let config = Path::new("C:/Users/me/AppData/Local/Strata/install/strata-iq3_xxs.json");
        let args = Terminal::serve_args(config, "0.0.0.0", "8080");
        assert_eq!(args[0], "serve");
        assert_eq!(Terminal::served_config(&args).as_deref(), Some(config));
        assert_eq!(Terminal::served_config(&["--native".into()]), None);
    }

    #[test]
    fn the_version_is_the_version_file_s_line() {
        let home = std::env::temp_dir().join(format!("terminal-version-{}", std::process::id()));
        let terminal = Terminal::new(&home);
        assert_eq!(terminal.version(), None);
        terminal.fake_install("0.2.2");
        assert!(terminal.is_installed());
        assert_eq!(terminal.version().as_deref(), Some("0.2.2"));
        std::fs::write(terminal.root().join("VERSION"), " \n").unwrap();
        assert_eq!(terminal.version(), None);
        std::fs::remove_dir_all(home).unwrap();
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
