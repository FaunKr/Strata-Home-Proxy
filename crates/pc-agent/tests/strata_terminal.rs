//! Jobs on Strata installed as the Strata Terminal, through the service's
//! HTTP interface: run configs in the Strata folder beside the Terminal,
//! Strata served with `strata serve`.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

fn config_json(gguf: &str) -> Value {
    json!({"exe": "component:strata-engine", "args": ["--native", gguf, "--max-context", "65536"],
        "port": 8080, "api_key": "key-0123456789abcdef", "reasoning_loop_recovery": "recover"})
}

/// The Terminal with IQ3_XXS (served) and coder-IQ1_M.
fn installed(s: &Setup) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves
        .config("iq3_xxs", config_json("a.gguf"))
        .config("coder-iq1_m", config_json("b.gguf"))
        .download("IQ3_XXS", "a.gguf", "0123456789")
        .download("coder-IQ1_M", "b.gguf", "01234");
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
    leaves
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

async fn job(s: &Setup, body: Value) -> Value {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

#[tokio::test]
async fn the_page_shows_the_terminal_and_the_served_model() {
    let s = setup("terminal-view").await;
    installed(&s);
    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], true);
    assert_eq!(view["install"], "terminal");
    assert_eq!(view["version"], "build 0.2.2", "before the first check");
    assert_eq!(model(&view, "IQ3_XXS")["active"], true);
    assert_eq!(model(&view, "coder-IQ1_M")["active"], false);
    assert_eq!(model(&view, "IQ3_XXS")["disk_bytes"], 10);
    assert_eq!(model(&view, "IQ3_XXS")["context"], 65536);
}

#[tokio::test]
async fn the_terminal_s_run_configs_win_over_an_old_strata_folder() {
    let s = setup("terminal-over-source").await;
    let cfg = &s.agent.cfg;
    SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef")
        .model("swift-iq2_xs", config_json("c.gguf"));
    installed(&s);
    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "terminal");
    let ids: Vec<_> = view["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    assert_eq!(ids, ["IQ3_XXS", "coder-IQ1_M"]);
}

#[tokio::test]
async fn another_model_installs_through_setup_in_its_install_mode() {
    let s = setup("terminal-add").await;
    installed(&s);
    let install = s.agent.cfg.strata_home.join("install");
    s.fake.command(
        "python",
        FakeCommand {
            creates: vec![(
                install.join("strata-swift-iq2_xs.json"),
                json!({"exe": "component:strata-engine", "args": ["--native"]}).to_string(),
            )],
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let setup = &s.fake.runs_of("python")[0];
    assert!(
        setup.args.contains(&"--setup".to_string()),
        "{:?}",
        setup.args
    );
    assert!(
        setup
            .args
            .windows(4)
            .any(|w| w == ["--family", "swift", "--model", "IQ2_XS"]),
        "{:?}",
        setup.args
    );
    assert!(
        setup
            .env
            .contains(&("STRATA_CONTROL_INSTALL".into(), "1".into()))
    );
    assert!(s.fake.runs_of("powershell").is_empty());
    assert!(
        s.fake.runs_of("strata.cmd").is_empty(),
        "the engine is there"
    );
    let added = terminal_folder(&s)
        .installation()
        .model("swift-iq2_xs")
        .unwrap();
    assert_eq!(added.loop_recovery(), "recover");
    assert_eq!(model(&get(&s, "/strata").await, "IQ3_XXS")["active"], true);
}

#[tokio::test]
async fn activating_serves_the_other_run_config() {
    let s = setup("terminal-activate").await;
    let leaves = installed(&s);
    let done = job(&s, json!({"kind": "activate", "model": "coder-IQ1_M"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, Terminal::of(&s.agent.cfg).program());
    let config = leaves.installation().config_path("coder-IQ1_M");
    assert_eq!(args, Terminal::serve_args(&config, "0.0.0.0", "8080"));
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped, Terminal::of(&s.agent.cfg).processes());
    assert_eq!(
        model(&get(&s, "/strata").await, "coder-IQ1_M")["active"],
        true
    );
    // The start brought a new key into the served run config.
    let key = leaves.installation().model("coder-iq1_m").unwrap().fields()["api_key"].clone();
    assert_ne!(key, "key-0123456789abcdef");
}

#[tokio::test]
async fn deleting_takes_the_run_config_and_the_model_files() {
    let s = setup("terminal-delete").await;
    let leaves = installed(&s);
    let done = job(&s, json!({"kind": "delete", "model": "coder-IQ1_M"})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(leaves.installation().models(), ["iq3_xxs"]);
    assert_eq!(leaves.installation().disk_bytes("coder-IQ1_M"), 0);
}

#[tokio::test]
async fn a_context_change_lands_in_the_run_config_and_restarts_strata() {
    let s = setup("terminal-context").await;
    let leaves = installed(&s);
    let done = job(
        &s,
        json!({"kind": "set_context", "model": "IQ3_XXS", "context": 131072}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        leaves.installation().model("iq3_xxs").unwrap().context(),
        Some(131072)
    );
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_speed_projection_takes_the_terminal_s_own_vector() {
    let s = setup("terminal-esp").await;
    let leaves = installed(&s);
    leaves.speed_projection_vector();
    let vector = format!("{}:1.0", leaves.vector().display());
    assert!(
        leaves
            .vector()
            .starts_with(Terminal::of(&s.agent.cfg).root())
    );
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "coder-IQ1_M", "on": true}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let args = leaves.installation().model("coder-iq1_m").unwrap().fields()["args"].clone();
    let args: Vec<_> = args
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        args.windows(2)
            .any(|w| w == ["--control-vector-scaled", vector.as_str()]),
        "{args:?}"
    );
    assert_eq!(
        model(&get(&s, "/strata").await, "coder-IQ1_M")["speed_projection"],
        true
    );
}

#[tokio::test]
async fn a_new_key_lands_in_the_served_run_config() {
    let s = setup("terminal-key").await;
    let leaves = installed(&s);
    let done = job(
        &s,
        json!({"kind": "set_key", "api_key": "proxy-key-0123456789"}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let key = leaves.installation().model("iq3_xxs").unwrap().fields()["api_key"].clone();
    assert_ne!(key, "key-0123456789abcdef");
    assert_ne!(key, "proxy-key-0123456789");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    let (_, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(
        Terminal::served_config(&args),
        Some(leaves.installation().config_path("iq3_xxs"))
    );
}

#[tokio::test]
async fn gaming_mode_stops_every_program_from_the_terminal_s_folder() {
    let s = setup("terminal-gaming").await;
    installed(&s);
    let (code, answer) = post(&s, "/gaming", json!({"enabled": true})).await;
    assert_eq!(code, 200, "{answer}");
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped, Terminal::of(&s.agent.cfg).processes());
}

/// What the options script printed with Strata 011c9e6 (see the unit test
/// in `terminal.rs`).
const OPTIONS: &str = include_str!("../fixtures/strata-options.json");

/// The page once it shows Strata's options for Terminal build `build`;
/// each visit may start the read, as the web UI's do.
async fn options_read(s: &Setup, build: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..200 {
        last = get(s, "/strata").await;
        if last["options"]["version"] == build {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no options on the page: {last}");
}

#[tokio::test]
async fn strata_s_options_are_read_once_per_terminal_version() {
    let s = setup("terminal-options").await;
    let leaves = installed(&s);
    s.fake.command(
        "python options",
        FakeCommand {
            lines: vec![OPTIONS.trim_end().to_string()],
            ..Default::default()
        },
    );
    let view = options_read(&s, "0.2.2").await;
    assert_eq!(s.fake.runs_of("python options").len(), 1);
    let options = &view["options"];
    assert_eq!(options["version"], "0.2.2");
    let spec = options["tuner"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["flag"] == "--spec")
        .unwrap();
    assert_eq!(spec["choices"], json!(["3", "4"]));
    assert_eq!(options["never"][0]["setting"], "STRATA_SH_STREAM");
    assert!(
        options["engine_help"]
            .as_str()
            .unwrap()
            .starts_with("strata generate")
    );
    let run = &s.fake.runs_of("python options")[0];
    let served = leaves.installation().config_path("iq3_xxs");
    assert_eq!(run.args.last().unwrap(), &served.display().to_string());

    get(&s, "/strata").await;
    get(&s, "/strata").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        s.fake.runs_of("python options").len(),
        1,
        "read again without a change"
    );

    Terminal::of(&s.agent.cfg).fake_install("0.2.3");
    let view = options_read(&s, "0.2.3").await;
    assert_eq!(view["options"]["version"], "0.2.3");
    assert_eq!(s.fake.runs_of("python options").len(), 2);
}

/// The options script's output with Strata 0.2.7's speed switches on a
/// PC with an RX 6000 card (see the unit tests in `terminal.rs`).
const OPTIONS_027: &str = include_str!("../fixtures/strata-options-0.2.7.json");

#[tokio::test]
async fn the_page_reports_speed_switches_and_documented_variables() {
    let s = setup("terminal-switches").await;
    installed(&s);
    s.fake.command(
        "python options",
        FakeCommand {
            lines: OPTIONS_027.lines().map(String::from).collect(),
            ..Default::default()
        },
    );
    let view = options_read(&s, "0.2.2").await;
    let options = &view["options"];
    assert_eq!(options["switches"][0]["label"], "Faster long prompts");
    assert_eq!(options["switches"][0]["env"], json!({"STRATA_KVA": "1"}));
    assert_eq!(
        options["switches"][0]["models"],
        json!(["coder-iq1_m", "iq3_s", "iq3_xxs", "q2_0"])
    );
    assert_eq!(options["documented"][0]["name"], "STRATA_HIP_PROMPT_F16");
    assert_eq!(options["documented"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn a_newly_installed_model_gets_the_options_read_again() {
    let s = setup("terminal-new-model").await;
    let leaves = installed(&s);
    s.fake.command(
        "python options",
        FakeCommand {
            lines: OPTIONS_027.lines().map(String::from).collect(),
            ..Default::default()
        },
    );
    options_read(&s, "0.2.2").await;
    assert_eq!(s.fake.runs_of("python options").len(), 1);

    leaves.config("swift-iq2_xs", config_json("c.gguf"));
    for _ in 0..200 {
        get(&s, "/strata").await;
        if s.fake.runs_of("python options").len() == 2 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the options were not read again after a new model");
}

#[tokio::test]
async fn a_strata_folder_from_source_has_no_options() {
    let s = setup("source-options").await;
    let cfg = &s.agent.cfg;
    SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef")
        .model("swift-iq2_xs", config_json("c.gguf"));
    get(&s, "/strata").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let view = get(&s, "/strata").await;
    assert!(view.get("options").is_none(), "{view}");
    assert!(s.fake.runs_of("python options").is_empty());
}

/// What `strata update --output jsonl` answers last (`run` in
/// strata_cli/update_cmd.py), with the Terminal's files at `control`.
fn update_answer(control: &str) -> String {
    format!(
        r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}}, "results": [{{"component": "engine", "status": "current", "installed_before": {{"version": "0.1.44.1"}}, "changed": false}}, {control}], "exit_code": 0}}"#
    )
}

fn answers(s: &Setup, name: &str, lines: &[&str], exit: Option<i32>) {
    s.fake.command(
        name,
        FakeCommand {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            exit,
            ..FakeCommand::default()
        },
    );
}

#[tokio::test]
async fn an_update_runs_strata_update_with_strata_stopped() {
    let s = setup("terminal-update").await;
    installed(&s);
    let control = r#"{"component": "control", "status": "updated", "changed": true, "installed_before": "0.1.44.1", "installed_after": "0.1.45"}"#;
    // Its events first, as `emit` in strata_control/updater.py writes them.
    let unpacking = r#"{"schema_version": 1, "event": "phase", "phase": "verify", "message": "Unpacking Control 0.1.45", "component": "control"}"#;
    let downloaded = r#"{"schema_version": 1, "event": "progress", "phase": "download", "component": "control", "label": "Control 0.1.45", "done": 81234567, "total": 81234567, "percent": 100, "bytes_per_second": 30000000}"#;
    answers(
        &s,
        "strata.cmd update",
        &[downloaded, unpacking, &update_answer(control)],
        None,
    );
    answers(
        &s,
        "strata.cmd --version",
        &[r"strata 0.2.3 (build 0.1.45; python 3.12.15 at C:\Strata\python\python.exe)"],
        None,
    );
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let update = &s.fake.runs_of("strata.cmd")[0];
    assert_eq!(update.args, ["update", "--output", "jsonl"]);
    // The phase is a line, no event or answer is.
    let shown = done["lines"].as_array().unwrap();
    assert!(shown.contains(&json!("Unpacking Control 0.1.45")), "{done}");
    assert!(
        !shown.iter().any(|l| l.as_str().unwrap().contains('{')),
        "{done}"
    );
    assert!(update.env.iter().any(|(k, _)| k == "STRATA_INSTALL_DIR"));
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        1,
        "stopped for the update"
    );
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1, "and started again");
    assert!(s.fake.runs_of("git").is_empty());
    let view = get(&s, "/strata").await;
    assert_eq!(view["version"], "0.2.3 (build 0.1.45)");
    assert_eq!(view["update_available"], false);
    // The same version after a restart, so a kept update summary stays.
    let again = strata_pc_agent::strata::view(&s.restarted());
    assert_eq!(again.info.version.as_deref(), Some("0.2.3 (build 0.1.45)"));
}

#[tokio::test]
async fn a_failed_update_says_why_and_strata_runs_again() {
    let s = setup("terminal-update-failed").await;
    installed(&s);
    let control = r#"{"component": "control", "status": "failed", "changed": false, "installed_before": "0.1.44.1", "error": "Control 0.1.45: the test start failed"}"#;
    answers(&s, "strata.cmd update", &[&update_answer(control)], Some(6));
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("Control 0.1.45: the test start failed"),
        "{done}"
    );
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        1,
        "the old Strata runs again"
    );
}

/// What `strata update --check --output json` answers (`check` in
/// strata_control/updater.py).
fn check_answer(control: &str) -> String {
    format!(
        r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 42, "channel": "stable", "key_id": "k", "generated_at": "2026-10-21T09:00:00Z", "trusted_by": "built-in keys"}}, "engine": {{"installed": {{"version": "0.1.44.1"}}, "offered": null, "reasons": [], "status": "current", "download_bytes": 0}}, "control": {{"installed": "0.1.44.1", "offered": "0.1.45", "status": "{control}", "download_bytes": 81234567}}, "app": {{"installed": null, "offered": null, "status": "not-in-feed", "notes": "", "download_bytes": 0}}, "serving": [], "hardware": {{}}}}"#
    )
}

const API: &str = "https://api.github.com/repos/Niko1221/Strata";

async fn view_once_checked(s: &Setup) -> Value {
    let mut view = get(s, "/strata").await;
    for _ in 0..100 {
        if view["update_available"] != Value::Null {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        view = get(s, "/strata").await;
    }
    view
}

fn installed_build(s: &Setup, commit: &str) {
    let root = Terminal::of(&s.agent.cfg).root();
    std::fs::write(root.join("COMMIT"), format!("{commit}\n")).unwrap();
}

#[tokio::test]
async fn the_update_check_asks_the_terminal_and_github_for_the_summary() {
    let s = setup("terminal-check").await;
    installed(&s);
    installed_build(&s, "76f90e58aaaa");
    answers(
        &s,
        "strata.cmd update --check",
        &[&check_answer("available")],
        None,
    );
    answers(
        &s,
        "strata.cmd --version",
        &[r"strata 0.2.2 (build 0.1.44.1; python 3.12.15 at C:\Strata\python\python.exe)"],
        None,
    );
    answers(
        &s,
        &format!("curl {API}/releases/latest"),
        &["{", r#"  "tag_name": "v0.2.3""#, "}"],
        None,
    );
    answers(
        &s,
        &format!("curl {API}/compare/76f90e58aaaa...v0.2.3"),
        &[
            "{",
            r#"  "status": "ahead", "total_commits": 1,"#,
            r#"  "commits": [{"sha": "011c9e60bbbb", "commit": {"committer": {"date": "2026-10-21T09:07:39Z"}, "message": "README: RX 6000 cards\n\nbuilt, not tested"}}],"#,
            r#"  "files": [{"filename": "README.md", "additions": 3, "deletions": 1}]"#,
            "}",
        ],
        None,
    );
    let view = view_once_checked(&s).await;
    assert_eq!(view["update_available"], true, "{view}");
    assert_eq!(view["version"], "0.2.2 (build 0.1.44.1)");
    let update = &view["update"];
    assert_eq!(update["upstream"], "011c9e60bbbb", "{view}");
    assert_eq!(update["total"], 1);
    assert_eq!(update["remote"], "https://github.com/Niko1221/Strata");
    assert_eq!(update["commits"][0]["subject"], "README: RX 6000 cards");
    assert_eq!(update["commits"][0]["time"], 1_792_573_659);
    assert_eq!(update["files"][0]["path"], "README.md");
    assert!(s.fake.runs_of("git").is_empty());
}

#[tokio::test]
async fn without_github_the_update_stays_available_without_a_summary() {
    let s = setup("terminal-check-offline").await;
    installed(&s);
    installed_build(&s, "76f90e58aaaa");
    answers(
        &s,
        "strata.cmd update --check",
        &[&check_answer("available")],
        None,
    );
    answers(
        &s,
        &format!("curl {API}/releases/latest"),
        &["curl: (6) Could not resolve host: api.github.com"],
        Some(6),
    );
    let view = view_once_checked(&s).await;
    assert_eq!(view["update_available"], true, "{view}");
    assert_eq!(view["update"], Value::Null);
}

#[tokio::test]
async fn nothing_new_asks_github_nothing() {
    let s = setup("terminal-check-current").await;
    installed(&s);
    answers(
        &s,
        "strata.cmd update --check",
        &[&check_answer("current")],
        None,
    );
    let view = view_once_checked(&s).await;
    assert_eq!(view["update_available"], false, "{view}");
    assert!(s.fake.runs_of("curl").is_empty());
}

/// What `strata bench run --output json` answers for the standard suite
/// (`_bench_standard` in strata_cli/cli.py, rows of `summaries_of` in
/// strata_cli/bench_standard.py).
const BENCH_ANSWER: &str = r#"{"schema_version": 1, "ok": true, "path": "C:\\bench\\results.json", "status": "complete", "quality": {}, "summaries": [{"phase": "performance", "case_id": "short", "concurrency": 1, "metric": "decode_tps", "unit": "tokens/s", "median": 38.54, "min": 37.1, "max": 39.2, "n": 3, "included_request_ids": [], "excluded": []}, {"phase": "performance", "case_id": "prompt-4k", "concurrency": 1, "metric": "prompt_tps", "unit": "tokens/s", "median": 612.0, "min": 600.0, "max": 620.0, "n": 3, "included_request_ids": [], "excluded": []}]}"#;

#[tokio::test]
async fn the_benchmark_is_strata_s_standard_one_against_the_running_strata() {
    let s = setup("terminal-bench").await;
    let leaves = installed(&s);
    answers(&s, "strata.cmd bench", &[BENCH_ANSWER], None);
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let run = &s.fake.runs_of("strata.cmd")[0];
    assert_eq!(
        run.args[..4],
        ["bench", "run", "--url", s.agent.cfg.strata_url.as_str()]
    );
    let out = &run.args[run.args.iter().position(|a| a == "--out").unwrap() + 1];
    assert!(
        out.starts_with(&s.agent.cfg.state_dir().display().to_string()),
        "{out}"
    );
    assert!(
        !std::path::Path::new(out).exists(),
        "Strata makes the folder itself"
    );
    // The key the served run config holds, so the benchmark gets in.
    let key = leaves.installation().model("iq3_xxs").unwrap().fields()["api_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(run.env.contains(&("STRATA_API_KEY".into(), key.clone())));
    assert!(!done.to_string().contains(key.as_str()));
    let result = &get(&s, "/strata").await["benchmark"];
    assert_eq!(result["result"], "38.5 Token/s");
    assert_eq!(result["prompt_tps"], 612.0);
    assert_eq!(result["tool"], "strata_bench");
    assert_eq!(result["version"], "build 0.2.2");
    assert!(s.fake.runs_of("BENCH.bat").is_empty());
}

#[tokio::test]
async fn a_benchmark_without_speeds_fails() {
    let s = setup("terminal-bench-none").await;
    installed(&s);
    answers(
        &s,
        "strata.cmd bench",
        &[
            r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "no server is answering at http://127.0.0.1:8080"}}"#,
        ],
        Some(5),
    );
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("no server is answering"),
        "{done}"
    );
}
