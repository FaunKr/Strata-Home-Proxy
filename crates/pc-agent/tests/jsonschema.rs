//! The Python package `jsonschema` in the Python that runs Strata: checked
//! and, when missing, installed with Strata's own command after an Install
//! or Update job and at the service's start.

use crate::common;

use std::{path::PathBuf, sync::atomic::Ordering};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{
    command::CommandSpec, install::testing::SetupLeaves, jsonschema, managed::Managed,
    terminal::Terminal,
};

const PIP: [&str; 8] = [
    "-m",
    "pip",
    "install",
    "--only-binary=:all:",
    "--disable-pip-version-check",
    "--no-input",
    "--no-warn-script-location",
    "jsonschema>=4.23,<5",
];

/// The Terminal in 0.2.2 with IQ3_XXS, served.
fn installed(s: &Setup) {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves
        .config(
            "iq3_xxs",
            json!({"exe": "component:strata-engine", "args": ["--native", "a.gguf"],
                "port": 8080, "api_key": "key-0123456789abcdef"}),
        )
        .download("IQ3_XXS", "a.gguf", "0123456789");
    let terminal = Terminal::of(cfg);
    let config = leaves.installation().config_path("iq3_xxs");
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&config, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
            ..Default::default()
        })
        .unwrap();
}

/// Strata's Python lacks `jsonschema`.
fn missing(s: &Setup) {
    s.fake.command(
        "python jsonschema",
        FakeCommand {
            lines: vec!["ModuleNotFoundError: No module named 'jsonschema'".into()],
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
}

fn pip_answers(s: &Setup, exit: Option<i32>) {
    s.fake.command(
        "python -m pip",
        FakeCommand {
            lines: vec![
                "Collecting jsonschema<5,>=4.23".into(),
                "Successfully installed".into(),
            ],
            exit,
            ..FakeCommand::default()
        },
    );
}

fn pips(s: &Setup) -> Vec<CommandSpec> {
    s.fake
        .runs_of("python")
        .into_iter()
        .filter(|c| c.args.first().is_some_and(|a| a == "-m"))
        .collect()
}

fn env<'a>(spec: &'a CommandSpec, name: &str) -> Option<&'a str> {
    spec.env
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The update answer of `strata update --output jsonl` with Control
/// unchanged (`strata_cli/update_cmd.py`).
const UPDATED: &str = r#"{"schema_version": 1, "ok": true, "feed": {"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}, "results": [{"component": "engine", "status": "updated", "installed_before": {"version": "0.1.44.1"}, "changed": true}, {"component": "control", "status": "current", "changed": false}], "exit_code": 0}"#;

async fn job(s: &Setup, body: Value) -> Value {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

fn shown(job: &Value) -> Vec<String> {
    job["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_update_installs_a_missing_jsonschema_before_strata_starts_again() {
    let s = setup("jsonschema-update").await;
    installed(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![UPDATED.into()],
            ..FakeCommand::default()
        },
    );
    missing(&s);
    pip_answers(&s, None);
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let python = Terminal::of(&s.agent.cfg).python();
    let check = &s.fake.runs_of("python jsonschema")[0];
    assert_eq!(check.program, python);
    let pip = &pips(&s)[0];
    assert_eq!(pip.program, python);
    assert_eq!(pip.args, PIP);
    assert_eq!(env(pip, "PYTHONNOUSERSITE"), Some("1"));
    assert_eq!(
        shown(&done)
            .iter()
            .filter(|l| l.contains("jsonschema"))
            .collect::<Vec<_>>(),
        [jsonschema::INSTALLED],
        "one line, without pip's output"
    );
    // pip ran while Strata was stopped, and Strata started once after it.
    let ran = s.fake.ran.lock().unwrap().clone();
    let update = ran
        .iter()
        .position(|c| c.args.first().is_some_and(|a| a == "update"))
        .unwrap();
    let pip_at = ran
        .iter()
        .position(|c| c.args.first().is_some_and(|a| a == "-m"))
        .unwrap();
    assert!(update < pip_at);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_update_with_jsonschema_there_shows_nothing_of_it() {
    let s = setup("jsonschema-there").await;
    installed(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![UPDATED.into()],
            ..FakeCommand::default()
        },
    );
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(s.fake.runs_of("python jsonschema").len(), 1, "checked");
    assert!(pips(&s).is_empty());
    assert!(
        !shown(&done).iter().any(|l| l.contains("jsonschema")),
        "{done}"
    );
}

#[tokio::test]
async fn a_failed_pip_leaves_the_update_done_and_says_why_in_one_line() {
    let s = setup("jsonschema-pip-failed").await;
    installed(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![UPDATED.into()],
            ..FakeCommand::default()
        },
    );
    missing(&s);
    pip_answers(&s, Some(1));
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let about: Vec<String> = shown(&done)
        .into_iter()
        .filter(|l| l.contains("jsonschema"))
        .collect();
    assert_eq!(about.len(), 1, "{done}");
    assert!(about[0].contains("pip ended with code 1"), "{done}");
    assert!(about[0].contains("only for one JSON object"), "{done}");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1, "Strata runs again");
}

#[tokio::test]
async fn an_update_that_brings_newer_terminal_files_checks_their_python() {
    let s = setup("jsonschema-control").await;
    installed(&s);
    let control = s.agent.cfg.strata_components.join("control");
    let folder = control.join("0.2.3");
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![UPDATED.into()],
            creates: vec![
                (folder.join("python").join("python.exe"), String::new()),
                (folder.join("strata.py"), String::new()),
                (
                    folder.join("strata_control").join("__init__.py"),
                    String::new(),
                ),
                (folder.join("VERSION"), "0.2.3\n".into()),
                (
                    control.join("active.json"),
                    json!({"folder": "0.2.3", "version": "0.2.3"}).to_string(),
                ),
            ],
            ..FakeCommand::default()
        },
    );
    missing(&s);
    pip_answers(&s, None);
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let python: PathBuf = folder.join("python").join("python.exe");
    assert_eq!(s.fake.runs_of("python jsonschema")[0].program, python);
    assert_eq!(pips(&s)[0].program, python);
}

#[tokio::test]
async fn an_update_of_a_strata_folder_from_source_uses_its_own_python() {
    let s = setup("jsonschema-source").await;
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    leaves.model(
        "iq3_xxs",
        json!({"exe": "engine\\strata.exe", "args": ["--native", "a.gguf"], "port": 8080}),
    );
    s.agent
        .set_managed(Managed {
            strata_start: Some(leaves.installation().run_script("iq3_xxs")),
            ..Default::default()
        })
        .unwrap();
    let venv = cfg
        .strata_dir
        .join(".venv")
        .join("Scripts")
        .join("python.exe");
    std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
    std::fs::write(&venv, "").unwrap();
    missing(&s);
    pip_answers(&s, None);
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(s.fake.runs_of("python jsonschema")[0].program, venv);
    assert_eq!(pips(&s)[0].program, venv);
    assert!(shown(&done).contains(&jsonschema::INSTALLED.to_string()));

    // Strata's own Python wins when the folder has one, as in START-HERE.bat.
    let bundled = cfg.strata_dir.join("python").join("python.exe");
    std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    std::fs::write(&bundled, "").unwrap();
    jsonschema::at_start(s.agent.clone()).await;
    assert_eq!(pips(&s)[1].program, bundled);
}

#[tokio::test]
async fn the_install_job_installs_jsonschema_into_the_new_terminal() {
    let s = setup("jsonschema-install").await;
    let terminal = s.agent.cfg.strata_home.join("terminal").join("current");
    s.fake.command(
        "powershell",
        FakeCommand {
            creates: vec![
                (terminal.join("bin").join("strata.cmd"), "@echo off".into()),
                (terminal.join("strata.py"), String::new()),
                (terminal.join("setup.py"), String::new()),
                (terminal.join("python").join("python.exe"), String::new()),
                (terminal.join("VERSION"), "0.2.2\n".into()),
            ],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![
                r#"{"schema_version": 1, "ok": true, "results": [{"component": "engine", "status": "updated", "changed": true}], "exit_code": 0}"#.into(),
            ],
            ..FakeCommand::default()
        },
    );
    missing(&s);
    pip_answers(&s, None);
    let done = job(
        &s,
        json!({"kind": "install", "api_key": "key-0123456789abcdef"}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let python = terminal.join("python").join("python.exe");
    assert_eq!(pips(&s)[0].program, python);
    assert_eq!(pips(&s)[0].args, PIP);
    assert!(shown(&done).contains(&jsonschema::INSTALLED.to_string()));
}

#[tokio::test]
async fn at_start_a_missing_jsonschema_is_installed() {
    let s = setup("jsonschema-start").await;
    installed(&s);
    missing(&s);
    pip_answers(&s, None);
    jsonschema::at_start(s.agent.clone()).await;
    let pip = &pips(&s)[0];
    assert_eq!(pip.program, Terminal::of(&s.agent.cfg).python());
    assert_eq!(pip.args, PIP);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0, "Strata runs on");
}

#[tokio::test]
async fn at_start_nothing_runs_during_a_job_or_without_strata() {
    let s = setup("jsonschema-start-job").await;
    missing(&s);
    jsonschema::at_start(s.agent.clone()).await;
    assert!(s.fake.runs_of("python jsonschema").is_empty(), "no Strata");

    installed(&s);
    let running = strata_pc_agent::job::testing::job(&s.agent);
    jsonschema::at_start(s.agent.clone()).await;
    assert!(s.fake.runs_of("python jsonschema").is_empty(), "a job runs");
    drop(running);
}

#[tokio::test]
async fn a_job_that_starts_during_the_start_check_ends_its_pip() {
    let s = setup("jsonschema-start-ended").await;
    installed(&s);
    missing(&s);
    // pip keeps printing and does not end on its own.
    s.fake.command(
        "python -m pip",
        FakeCommand {
            lines: (0..10_000)
                .map(|i| format!("Downloading part {i}"))
                .collect(),
            pause: std::time::Duration::from_millis(1),
            hang: true,
            ..FakeCommand::default()
        },
    );
    let check = tokio::spawn(jsonschema::at_start(s.agent.clone()));
    eventually("pip runs", || !pips(&s).is_empty()).await;
    let running = strata_pc_agent::job::testing::job(&s.agent);
    tokio::time::timeout(std::time::Duration::from_secs(5), check)
        .await
        .expect("the start check ends once a job runs")
        .unwrap();
    drop(running);
}
