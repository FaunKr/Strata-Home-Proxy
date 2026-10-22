//! Strata's presets (`strata presets`) through the service's HTTP interface
//! with a fake `strata`: each model's preset line, Apply, Undo and Off.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

/// What `strata presets status --output json` printed (see the unit test in
/// `terminal.rs`): an active preset Strata offers to re-check.
const STATUS: &str = include_str!("../fixtures/strata-presets-status.json");
const PRESET: &str = "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6";

/// The Terminal with IQ3_XXS (served) and coder-IQ1_M.
fn installed(s: &Setup) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    let config = |gguf: &str| {
        json!({"exe": "component:strata-engine", "args": ["--native", gguf, "--max-context", "65536"],
            "port": 8080, "api_key": "key-0123456789abcdef"})
    };
    leaves.config("iq3_xxs", config("a.gguf"));
    leaves.config("coder-iq1_m", config("b.gguf"));
    let terminal = Terminal::of(cfg);
    let served = leaves.installation().config_path("iq3_xxs");
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&served, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
        })
        .unwrap();
    leaves
}

fn presets_say(s: &Setup, line: &str) {
    s.fake.command(
        "strata.cmd presets",
        FakeCommand {
            lines: vec![line.to_string()],
            ..FakeCommand::default()
        },
    );
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

/// The page, once model `id` shows its presets.
async fn presets_of(s: &Setup, id: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..200 {
        let view = get(s, "/strata").await;
        last = model(&view, id)["presets"].clone();
        if !last.is_null() {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no presets for {id}, last: {last}");
}

#[tokio::test]
async fn each_model_shows_its_latest_preset_with_strata_s_verdict() {
    let s = setup("presets-line").await;
    let leaves = installed(&s);
    presets_say(&s, STATUS.trim_end());
    let presets = presets_of(&s, "IQ3_XXS").await;
    let latest = &presets["latest"];
    assert_eq!(latest["id"], PRESET);
    assert_eq!(latest["active"], true);
    assert_eq!(latest["state"], "soft");
    assert_eq!(latest["reasons"], json!(["the graphics driver changed"]));
    assert_eq!(presets["other_context"], false);

    let config = leaves
        .installation()
        .config_path("iq3_xxs")
        .display()
        .to_string();
    let asked = s.fake.runs_of("strata.cmd");
    assert!(
        asked
            .iter()
            .any(|run| run.args == ["presets", "status", &config, "--output", "json"]),
        "{asked:?}"
    );
    // Read once: more visits ask Strata no more.
    presets_of(&s, "coder-IQ1_M").await;
    let before = s.fake.runs_of("strata.cmd").len();
    get(&s, "/strata").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.fake.runs_of("strata.cmd").len(), before);

    // A context change makes it read again.
    let (code, _) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 131072}),
    )
    .await;
    assert_eq!(code, 202);
    assert_eq!(finished(&s).await["state"], "done");
    let asked = s.fake.runs_of("strata.cmd").len();
    get(&s, "/strata").await;
    eventually("the presets asked again", || {
        s.fake.runs_of("strata.cmd").len() > asked
    })
    .await;
}

#[tokio::test]
async fn applying_a_preset_restarts_the_active_model() {
    let s = setup("presets-apply").await;
    installed(&s);
    presets_say(
        &s,
        r#"{"schema_version": 1, "ok": true, "preset": {}, "notice": ""}"#,
    );
    let body = json!({"kind": "apply_preset", "model": "IQ3_XXS", "preset": PRESET});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    let runs = s.fake.runs_of("strata.cmd");
    assert_eq!(
        runs[0].args,
        ["presets", "apply", PRESET, "--output", "json"]
    );
    eventually("Strata restarted", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;

    // Undo, for a model that does not run: no restart.
    let body = json!({"kind": "undo_preset", "model": "coder-IQ1_M", "preset": PRESET});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 202);
    assert_eq!(finished(&s).await["state"], "done");
    let runs = s.fake.runs_of("strata.cmd");
    assert!(
        runs.iter()
            .any(|run| run.args == ["presets", "undo", PRESET, "--output", "json"]),
        "{runs:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn switching_a_preset_off_restarts_the_active_model() {
    let s = setup("presets-off").await;
    let leaves = installed(&s);
    presets_say(&s, r#"{"schema_version": 1, "ok": true, "active": null}"#);
    let body = json!({"kind": "preset_off", "model": "IQ3_XXS"});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    let config = leaves
        .installation()
        .config_path("iq3_xxs")
        .display()
        .to_string();
    let runs = s.fake.runs_of("strata.cmd");
    assert_eq!(
        runs[0].args,
        ["presets", "off", &config, "--output", "json"]
    );
    eventually("Strata restarted", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn a_refused_apply_fails_with_strata_s_reason() {
    let s = setup("presets-refused").await;
    installed(&s);
    presets_say(
        &s,
        r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "there is nothing to undo"}}"#,
    );
    let body = json!({"kind": "undo_preset", "model": "IQ3_XXS", "preset": PRESET});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(done["error"], "there is nothing to undo");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0, "no restart");
}

#[tokio::test]
async fn presets_are_refused_for_a_folder_from_source_and_for_other_ids() {
    let s = setup("presets-bad").await;
    installed(&s);
    let body = json!({"kind": "apply_preset", "model": "IQ3_XXS", "preset": "--all"});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 400);

    let s = setup("presets-source").await;
    let cfg = &s.agent.cfg;
    SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef").model(
        "iq3_xxs",
        json!({"exe": "strata.exe", "args": ["--native", "a.gguf"], "port": 8080}),
    );
    let body = json!({"kind": "apply_preset", "model": "IQ3_XXS", "preset": PRESET});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 409, "{answer}");
    let view = get(&s, "/strata").await;
    assert!(model(&view, "IQ3_XXS").get("presets").is_none(), "{view}");
    assert!(s.fake.runs_of("strata.cmd").is_empty());
}
