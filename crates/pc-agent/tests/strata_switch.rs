//! The switch from an old Strata folder from source to the Strata Terminal,
//! and removing the old folder afterwards, through the service's HTTP
//! interface with a fake install script, Terminal and setup.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, platform::folder_entry, terminal::Terminal};

/// What `strata update --output json` prints for the engine's `status`
/// (`strata_cli/update_cmd.py`, `run`, Strata 011c9e6).
fn engine_answer(status: &str) -> String {
    format!(
        r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 41, "channel": "stable", "key_id": "k"}}, "results": [{{"component": "engine", "status": "{status}", "installed_before": null, "changed": true, "note": "The feed has no engine package for this PC.", "reasons": ["hip gfx1030: no package"]}}], "exit_code": 0}}"#
    )
}

const CHAT: &str = r#"{"temperature": 0.6}"#;

/// The old Strata folder serving IQ3_XXS with 128K context, the speed
/// projection and Chat settings, its files in the Strata data folder.
fn old_folder(s: &Setup) -> Managed {
    old_folder_with(
        s,
        json!([
            "--native",
            "a.gguf",
            "--max-context",
            "131072",
            "--control-vector-scaled",
            "C:\\Strata\\data\\v.gguf",
            "0.6"
        ]),
    )
}

/// The old Strata folder serving IQ3_XXS with these engine arguments.
fn old_folder_with(s: &Setup, args: Value) -> Managed {
    old_folder_config(s, json!({"exe": "strata.exe", "args": args, "port": 8080}))
}

/// The old Strata folder serving IQ3_XXS with this run config.
fn old_folder_config(s: &Setup, config: Value) -> Managed {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    leaves
        .model("iq3_xxs", config)
        .chat_settings("iq3_xxs", CHAT)
        .download("IQ3_XXS", "a.gguf", "0123456789");
    let managed = Managed {
        strata_start: Some(leaves.installation().run_script("iq3_xxs")),
        strata_args: vec![],
        strata_processes: Some(vec![folder_entry(&cfg.strata_dir)]),
    };
    s.agent.set_managed(managed.clone()).unwrap();
    managed
}

/// The install script puts the Terminal in place, its update brings the
/// engine, and its setup writes the run config.
fn switchable(s: &Setup) {
    let terminal = s.agent.cfg.strata_home.join("terminal").join("current");
    let install = s.agent.cfg.strata_home.join("install");
    let created = |path: &str| (terminal.join(path), String::new());
    s.fake.command(
        "powershell",
        FakeCommand {
            creates: vec![
                (terminal.join("bin").join("strata.cmd"), "@echo off".into()),
                created("strata.py"),
                created("setup.py"),
                created("python/python.exe"),
                (terminal.join("VERSION"), "0.2.2\n".into()),
            ],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![engine_answer("updated")],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "python",
        FakeCommand {
            lines: vec!["  a.gguf already downloaded".into()],
            creates: vec![(
                install.join("strata-iq3_xxs.json"),
                json!({"exe": "component:strata-engine", "args": ["--native", "a.gguf", "--max-context", "131072"], "port": 8080})
                    .to_string(),
            )],
            ..FakeCommand::default()
        },
    );
}

async fn job(s: &Setup, kind: &str) -> Value {
    let (code, answer) = post(s, "/strata/jobs", json!({ "kind": kind })).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

async fn refused(s: &Setup, kind: &str) -> String {
    let (code, answer) = post(s, "/strata/jobs", json!({ "kind": kind })).await;
    assert_eq!(code, 409, "{answer}");
    answer["reason"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_switch_serves_the_active_model_on_the_terminal_with_its_settings() {
    let s = setup("switch").await;
    let old = old_folder(&s);
    switchable(&s);
    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "source");
    assert_eq!(view["old_strata_folder"], true);
    assert_eq!(view["terminal_served"], false);

    let done = job(&s, "switch_to_terminal").await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["model"], "IQ3_XXS");
    let cfg = &s.agent.cfg;
    let terminal = Terminal::of(cfg);

    // The Terminal's setup in its install mode on the same data folder,
    // with the model's context and speed projection.
    let setup = &s.fake.runs_of("python")[0];
    let data = cfg.strata_data_dir.display().to_string();
    for wanted in [
        ["--family", "qwen"],
        ["--model", "IQ3_XXS"],
        ["--context", "131072"],
        ["--data-dir", data.as_str()],
        ["--experimental-speed-projection", "on"],
    ] {
        assert!(
            setup.args.windows(2).any(|w| w == wanted),
            "{wanted:?}: {:?}",
            setup.args
        );
    }
    assert!(
        !setup.args.contains(&"--setup".to_string()),
        "{:?}",
        setup.args
    );
    assert!(
        setup
            .env
            .contains(&("STRATA_CONTROL_INSTALL".into(), "1".into()))
    );

    // The old Strata stopped, `strata serve` started with the new run config.
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped, old.strata_processes.unwrap());
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, terminal.program());
    let config = terminal_folder(&s).installation().config_path("iq3_xxs");
    assert_eq!(args, Terminal::serve_args(&config, "0.0.0.0", "8080"));

    // The run config is managed and the Chat settings came along.
    let adopted = terminal_folder(&s).installation().model("iq3_xxs").unwrap();
    assert_eq!(adopted.loop_recovery(), "recover");
    assert_eq!(terminal_folder(&s).chat_text("iq3_xxs"), CHAT);

    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "terminal");
    assert_eq!(view["old_strata_folder"], true);
    assert_eq!(view["terminal_served"], true);
    assert_eq!(view["models"][0]["id"], "IQ3_XXS");
    assert_eq!(view["models"][0]["active"], true);
    assert_eq!(view["models"][0]["disk_bytes"], 10, "the same model files");
}

#[tokio::test]
async fn without_an_engine_for_this_pc_the_old_strata_keeps_serving() {
    let s = setup("switch-no-engine").await;
    let old = old_folder(&s);
    switchable(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![engine_answer("none-for-this-pc")],
            ..FakeCommand::default()
        },
    );
    let done = job(&s, "switch_to_terminal").await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(
        done["error"],
        "The feed has no engine package for this PC. (hip gfx1030: no package)"
    );
    assert!(s.fake.runs_of("python").is_empty(), "no setup");
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        0,
        "Strata never stopped"
    );
    assert_eq!(s.agent.managed(), old);
    assert_eq!(get(&s, "/strata").await["install"], "source");
}

#[tokio::test]
async fn a_terminal_strata_that_does_not_start_brings_the_old_start_back() {
    let s = setup("switch-no-start").await;
    let old = old_folder(&s);
    switchable(&s);
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let done = job(&s, "switch_to_terminal").await;
    assert_eq!(done["state"], "failed", "{done}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("the old Strata folder serves again"),
        "{done}"
    );
    assert_eq!(s.agent.managed(), old);
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(
        program,
        old.strata_start.unwrap(),
        "the old Strata started again"
    );
    assert!(terminal_folder(&s).installation().models().is_empty());
    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "source");
    assert_eq!(view["terminal_served"], false);
    // The model files stay.
    assert_eq!(view["models"][0]["disk_bytes"], 10);
}

#[tokio::test]
async fn the_switch_is_only_for_an_old_strata_folder() {
    let s = setup("switch-terminal").await;
    SetupLeaves::terminal(
        &s.agent.cfg.strata_home,
        &s.agent.cfg.strata_data_dir,
        "0.2.2",
    )
    .config(
        "iq3_xxs",
        json!({"exe": "component:strata-engine", "args": []}),
    );
    assert_eq!(
        refused(&s, "switch_to_terminal").await,
        "Strata runs on the Strata Terminal already"
    );
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_old_folder_stays_until_the_terminal_s_strata_has_served() {
    let s = setup("switch-remove-early").await;
    old_folder(&s);
    let reason = refused(&s, "remove_old_strata_folder").await;
    assert!(
        reason.contains("switch to the Strata Terminal first"),
        "{reason}"
    );
    SetupLeaves::terminal(
        &s.agent.cfg.strata_home,
        &s.agent.cfg.strata_data_dir,
        "0.2.2",
    )
    .config(
        "iq3_xxs",
        json!({"exe": "component:strata-engine", "args": []}),
    );
    let reason = refused(&s, "remove_old_strata_folder").await;
    assert!(reason.contains("has not served yet"), "{reason}");
    assert!(s.agent.cfg.strata_dir.join("setup.py").is_file());
}

#[tokio::test]
async fn removing_the_old_folder_leaves_the_data_folder_inside_it() {
    let s = setup_with("switch-remove", |cfg| {
        cfg.strata_data_dir = cfg.strata_dir.join("models-data");
    })
    .await;
    old_folder(&s);
    switchable(&s);
    assert_eq!(job(&s, "switch_to_terminal").await["state"], "done");

    let done = job(&s, "remove_old_strata_folder").await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = &s.agent.cfg;
    assert!(!cfg.strata_dir.join("setup.py").exists());
    assert!(!cfg.strata_dir.join("strata-iq3_xxs.json").exists());
    assert!(!cfg.strata_dir.join(".git").exists());
    assert!(
        cfg.strata_data_dir
            .join("models")
            .join("IQ3_XXS")
            .join("a.gguf")
            .is_file()
    );
    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "terminal");
    assert_eq!(view["old_strata_folder"], false);
    assert_eq!(view["models"][0]["disk_bytes"], 10);
    assert_eq!(
        refused(&s, "remove_old_strata_folder").await,
        "There is no old Strata folder"
    );
}

#[tokio::test]
async fn a_cancel_while_strata_is_busy_leaves_the_old_strata_serving() {
    let s = setup("switch-cancel").await;
    let old = old_folder(&s);
    switchable(&s);
    switch_waiting(&s).await;
    assert_eq!(post(&s, "/strata/job/cancel", json!({})).await.0, 202);
    assert_eq!(finished(&s).await["state"], "cancelled");
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        0,
        "Strata never stopped"
    );
    assert_eq!(s.agent.managed(), old);
    assert!(terminal_folder(&s).installation().models().is_empty());
    assert_eq!(get(&s, "/strata").await["install"], "source");
}

#[tokio::test]
async fn the_switch_is_refused_in_gaming_mode() {
    let s = setup("switch-gaming").await;
    old_folder(&s);
    s.agent.set_gaming(true);
    assert_eq!(
        refused(&s, "switch_to_terminal").await,
        "Gaming mode is on: switch it off first"
    );
}

#[tokio::test]
async fn the_watchdog_notes_when_the_terminal_s_strata_answers() {
    let s = setup("switch-served").await;
    old_folder(&s);
    s.agent.watchdog_tick().await;
    assert_eq!(
        get(&s, "/strata").await["terminal_served"],
        false,
        "the old Strata answered"
    );
    let cfg = &s.agent.cfg;
    let terminal = Terminal::of(cfg);
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves.config(
        "iq3_xxs",
        json!({"exe": "component:strata-engine", "args": []}),
    );
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(
                &leaves.installation().config_path("iq3_xxs"),
                "0.0.0.0",
                "8080",
            ),
            strata_processes: Some(terminal.processes()),
        })
        .unwrap();
    s.agent.watchdog_tick().await;
    assert_eq!(get(&s, "/strata").await["terminal_served"], true);
}

/// Starts the switch and waits until it waits for Strata's running request,
/// with the Terminal's run config written.
async fn switch_waiting(s: &Setup) {
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, answer) = post(s, "/strata/jobs", json!({"kind": "switch_to_terminal"})).await;
    assert_eq!(code, 202, "{answer}");
    job_until(s, "waiting for the request", |j| {
        j["step"]
            .as_str()
            .is_some_and(|step| step.contains("running request"))
    })
    .await;
    assert_eq!(terminal_folder(s).installation().models(), ["iq3_xxs"]);
}

#[tokio::test]
async fn until_the_terminal_serves_the_page_shows_the_old_folder() {
    let s = setup("switch-midway").await;
    let old = old_folder(&s);
    switchable(&s);
    switch_waiting(&s).await;
    let view = get(&s, "/strata").await;
    assert_eq!(view["install"], "source", "{view}");
    assert_eq!(view["models"][0]["active"], true, "{view}");

    // The service ends here; the next one undoes the switch.
    let again = s.restarted();
    // What is left to do afterwards (a restart of Strata) does not matter here.
    drop(strata_pc_agent::strata::manage_configs_at_start(&again).await);
    assert_eq!(again.managed(), old);
    assert!(terminal_folder(&s).installation().models().is_empty());
    let view = serde_json::to_value(strata_pc_agent::strata::view(&again)).unwrap();
    assert_eq!(view["install"], "source", "{view}");
}

#[tokio::test]
async fn gaming_mode_during_the_switch_leaves_the_old_start() {
    let s = setup("switch-gaming-midway").await;
    let old = old_folder(&s);
    switchable(&s);
    switch_waiting(&s).await;
    s.agent.set_gaming(true);
    assert_eq!(finished(&s).await["state"], "cancelled");
    assert_eq!(s.agent.managed(), old);
    assert!(terminal_folder(&s).installation().models().is_empty());
    assert_eq!(get(&s, "/strata").await["install"], "source");
}

#[tokio::test]
async fn a_model_without_a_context_switches_with_the_default_one() {
    let s = setup("switch-default-context").await;
    old_folder_with(&s, json!(["--native", "a.gguf"]));
    switchable(&s);
    assert_eq!(job(&s, "switch_to_terminal").await["state"], "done");
    let setup = &s.fake.runs_of("python")[0];
    for wanted in [
        ["--context", "65536"],
        ["--experimental-speed-projection", "off"],
        ["--vision", "no"],
    ] {
        assert!(
            setup.args.windows(2).any(|w| w == wanted),
            "{wanted:?}: {:?}",
            setup.args
        );
    }
}

#[tokio::test]
async fn a_model_with_picture_reading_keeps_it_in_the_switch() {
    let s = setup("switch-vision").await;
    // A run config with setup's vision section, as an install with pictures
    // leaves it.
    old_folder_config(
        &s,
        json!({"exe": "strata.exe", "args": ["--native", "a.gguf"], "port": 8080,
            "vision": {"exe": "strata-vision-hip.exe", "mmproj": "mmproj.gguf",
                "model": "a.gguf", "gpu": true, "max_tokens": 1024}}),
    );
    switchable(&s);
    assert_eq!(job(&s, "switch_to_terminal").await["state"], "done");
    let setup = &s.fake.runs_of("python")[0];
    assert!(
        setup.args.windows(2).any(|w| w == ["--vision", "yes"]),
        "{:?}",
        setup.args
    );
}
