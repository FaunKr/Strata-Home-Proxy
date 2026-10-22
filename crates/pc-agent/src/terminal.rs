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

use crate::command::CommandSpec;

/// The install script's folder for the Terminal itself, a link to the
/// version installed last.
const CURRENT: [&str; 2] = ["terminal", "current"];
/// The Strata folder with the run configs, beside the engines, where
/// Strata's desktop app keeps them too.
const INSTALL: &str = "install";
/// The `strata` command in the Terminal's own files.
const PROGRAM: [&str; 2] = ["bin", "strata.cmd"];
/// Where `strata update` keeps the engines and newer Terminal files
/// ("Control"); Strata's own default beside `install`.
const COMPONENTS: &str = "components";

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
        let base = self.home.join(COMPONENTS).join("control");
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
        let text = std::fs::read_to_string(self.root().join("COMMIT")).ok()?;
        Some(text.trim().to_string()).filter(|c| !c.is_empty())
    }

    /// `strata --version`; read it with [`version_text`].
    pub fn version(&self) -> CommandSpec {
        self.command(&["--version"])
    }

    /// The engine and the Terminal's files from the signed feed, each only
    /// after a test start. Read the output with [`updated`].
    pub fn update(&self) -> CommandSpec {
        self.command(&["update", "--output", "json"])
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

/// Reads what `strata update --output json` printed: `Ok` when the engine
/// for this PC is in place, else Strata's reason. The answer is the last
/// line that is a JSON object (`strata_cli/update_cmd.py`, `run`).
pub fn engine_installed(output: &[String]) -> Result<(), String> {
    let answer =
        json_answer(output).ok_or("The Strata Terminal gave no answer about the engine")?;
    if let Some(error) = answer.get("error") {
        return Err(text(&error["message"])
            .unwrap_or("The engine update failed")
            .to_string());
    }
    let engine = answer["results"]
        .as_array()
        .and_then(|r| r.iter().find(|r| r["component"] == "engine"))
        .ok_or("The Strata Terminal's answer names no engine")?;
    part_problem(engine).map_or(Ok(()), Err)
}

fn read_version(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(folder.join("VERSION")).ok()?;
    Some(text.trim().to_string()).filter(|v| !v.is_empty())
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

/// The last line of `output` that is a JSON object: what `--output json`
/// answers.
fn json_answer(output: &[String]) -> Option<Value> {
    output.iter().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .filter(Value::is_object)
    })
}

/// Reads what `strata update --check --output json` printed: whether the
/// engine or the Terminal's files have something newer for this PC
/// (`check` in `strata_control/updater.py`).
pub fn update_available(output: &[String]) -> Result<bool, String> {
    let answer = json_answer(output).ok_or("The Strata Terminal gave no answer about updates")?;
    if let Some(error) = answer.get("error") {
        return Err(text(&error["message"])
            .unwrap_or("The update check failed")
            .to_string());
    }
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
    if let Some(error) = answer.get("error") {
        return Err(text(&error["message"])
            .unwrap_or("The update failed")
            .to_string());
    }
    let results = answer["results"]
        .as_array()
        .ok_or("The Strata Terminal's answer names no parts")?;
    let problems: Vec<String> = results.iter().filter_map(part_problem).collect();
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
    if let Some(error) = answer.get("error") {
        return Err(text(&error["message"])
            .unwrap_or("The benchmark failed")
            .to_string());
    }
    let median = |case: &str, metric: &str| {
        answer["summaries"].as_array()?.iter().find_map(|row| {
            (row["phase"] == "performance" && row["case_id"] == case && row["metric"] == metric)
                .then(|| row["median"].as_f64())
                .flatten()
        })
    };
    let decode = median("short", "decode_tps").ok_or_else(|| {
        let status = text(&answer["status"]).unwrap_or("unknown");
        format!("The benchmark reported no tokens/s (run {status})")
    })?;
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
        let nothing = answer("failed", &[row("short", "decode_tps", "null")]);
        assert_eq!(
            bench_speeds(&nothing),
            Err("The benchmark reported no tokens/s (run failed)".into())
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
