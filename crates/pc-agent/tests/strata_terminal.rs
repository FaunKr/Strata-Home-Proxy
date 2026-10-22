//! Jobs on Strata installed as the Strata Terminal, through the service's
//! HTTP interface: run configs in the Strata folder beside the Terminal,
//! Strata served with `strata serve`.

use crate::common;

use std::sync::atomic::Ordering;

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
    assert_eq!(view["version"], "0.2.2");
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
async fn update_benchmark_and_comparison_are_not_offered_yet() {
    let s = setup("terminal-not-yet").await;
    installed(&s);
    for kind in ["update", "benchmark", "compare_models"] {
        let (code, answer) = post(&s, "/strata/jobs", json!({"kind": kind})).await;
        assert_eq!(code, 409, "{kind}: {answer}");
        assert!(
            answer["reason"]
                .as_str()
                .unwrap()
                .contains("Strata Terminal"),
            "{kind}: {answer}"
        );
    }
    assert!(s.fake.ran.lock().unwrap().is_empty());
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

/// The page once it shows Strata's options for its version; each visit
/// may start the read, as the web UI's do.
async fn options_read(s: &Setup) -> Value {
    let mut last = Value::Null;
    for _ in 0..200 {
        last = get(s, "/strata").await;
        if last["options"]["version"] == last["version"] {
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
        "python -c",
        FakeCommand {
            lines: vec![OPTIONS.trim_end().to_string()],
            ..Default::default()
        },
    );
    let view = options_read(&s).await;
    assert_eq!(s.fake.runs_of("python").len(), 1);
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
    let run = &s.fake.runs_of("python")[0];
    let served = leaves.installation().config_path("iq3_xxs");
    assert_eq!(run.args.last().unwrap(), &served.display().to_string());

    get(&s, "/strata").await;
    get(&s, "/strata").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        s.fake.runs_of("python").len(),
        1,
        "read again without a change"
    );

    Terminal::of(&s.agent.cfg).fake_install("0.2.3");
    let view = options_read(&s).await;
    assert_eq!(view["options"]["version"], "0.2.3");
    assert_eq!(s.fake.runs_of("python").len(), 2);
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
    assert!(s.fake.runs_of("python").is_empty());
}
