//! Tuning a model with Strata's auto-tuner (`strata tune auto`) as a Strata
//! job, through the service's HTTP interface with a fake `strata`.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

/// What `strata tune auto --output json` printed for a faster result (see
/// the unit test in `terminal.rs`).
const TUNED: &str = include_str!("../fixtures/strata-tune.json");
/// The preset Strata saved it as.
const PRESET: &str = "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6";

/// The Terminal with IQ3_XXS (served) and qwen36-Q4_K_M.
fn installed(s: &Setup) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    let config = |gguf: &str| {
        json!({"exe": "component:strata-engine", "args": ["--native", gguf, "--max-context", "65536"],
            "port": 8080, "api_key": "key-0123456789abcdef"})
    };
    leaves.config("iq3_xxs", config("a.gguf"));
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

fn tuner_says(s: &Setup, lines: &[&str], pause: Duration, hang: bool) {
    s.fake.command(
        "strata.cmd tune",
        FakeCommand {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            pause,
            hang,
            exit: Some(0),
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "strata.cmd presets",
        FakeCommand {
            lines: vec![r#"{"schema_version": 1, "ok": true, "active": null}"#.into()],
            ..FakeCommand::default()
        },
    );
}

fn tune(model: &str) -> Value {
    json!({"kind": "tune", "model": model, "mode": "thorough", "use": "chat"})
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

#[tokio::test]
async fn a_tune_runs_the_tuner_with_strata_stopped_and_keeps_its_result() {
    let s = setup("tune-runs").await;
    let leaves = installed(&s);
    tuner_says(
        &s,
        &["Tuning qwen3.8 (thorough)", TUNED.trim_end()],
        Duration::from_millis(1500),
        false,
    );
    let (code, answer) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(get(&s, "/status").await["strata_job_kind"], "tune");

    // The tuner's status file feeds the job's step, share and time left.
    let status = s.agent.cfg.state_dir().join("strata-tune-status.json");
    eventually("the tuner started", || {
        !s.fake.runs_of("strata.cmd").is_empty()
    })
    .await;
    std::fs::write(
        &status,
        r#"{"stage": "arm", "message": "Trying: Shorter speculative window (3 tokens)", "state": "running", "elapsed_s": 420, "runs_done": 3, "runs_planned": 8, "eta_s": 750}"#,
    )
    .unwrap();
    let running = job_until(&s, "the tuner's progress", |j| j["percent"] == 37).await;
    assert_eq!(
        running["step"],
        "Trying: Shorter speculative window (3 tokens)"
    );
    assert_eq!(running["eta_secs"], 750);

    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        done["result"],
        "31.2 → 35.1 Token/s (+12%): --kv-resident 65536, --kv-grow"
    );
    let runs = s.fake.runs_of("strata.cmd");
    let config = leaves
        .installation()
        .config_path("iq3_xxs")
        .display()
        .to_string();
    assert_eq!(
        runs[0].args,
        [
            "tune",
            "auto",
            &config,
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
    // The preset Strata made active goes back: nothing is applied.
    assert_eq!(
        runs[1].args,
        ["presets", "undo", PRESET, "--output", "json"]
    );
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        1,
        "stopped for the tune"
    );
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1, "and started again");

    let view = get(&s, "/strata").await;
    let iq3 = model(&view, "IQ3_XXS");
    assert_eq!(iq3["tunable"], true);
    let last = &iq3["last_tune"];
    assert_eq!(last["code"], "faster");
    assert_eq!(last["mode"], "thorough");
    assert_eq!(last["use"], "chat");
    assert_eq!(last["tuned_tps"], 35.1);
    assert_eq!(last["preset"], PRESET);
    // Kept over a restart of the service.
    let again = strata_pc_agent::strata::view(&s.restarted());
    assert_eq!(
        again.info.models[0].last_tune.as_ref().unwrap().code,
        "faster"
    );
}

#[tokio::test]
async fn a_tune_that_finds_nothing_faster_says_so_and_undoes_nothing_unsaved() {
    let s = setup("tune-same").await;
    installed(&s);
    tuner_says(
        &s,
        &[
            r#"{"schema_version": 1, "ok": true, "result": {"outcome": "inconclusive", "code": "busy_pc", "reason_code": "noisy", "values": {"gain_pct": 0.0, "base_tok_s": 30.9, "tuned_tok_s": null}, "message": "Your PC was too busy to compare settings. Nothing was changed.", "settings": {"args": {}, "env": {}, "per_request": {}}, "preset": null}}"#,
        ],
        Duration::ZERO,
        false,
    );
    let (code, _) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        done["result"],
        "Your PC was too busy to compare settings. Nothing was changed."
    );
    assert_eq!(s.fake.runs_of("strata.cmd").len(), 1, "no preset to undo");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_a_tune_starts_strata_again_and_keeps_nothing() {
    let s = setup("tune-cancel").await;
    installed(&s);
    tuner_says(&s, &["Tuning qwen3.8 (quick)"], Duration::ZERO, true);
    let (code, _) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202);
    eventually("the tuner started", || {
        !s.fake.runs_of("strata.cmd").is_empty()
    })
    .await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "cancelled", "{done}");
    assert_eq!(s.fake.runs_of("strata.cmd").len(), 1, "no preset to undo");
    eventually("Strata started again", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
    let view = get(&s, "/strata").await;
    assert!(model(&view, "IQ3_XXS").get("last_tune").is_none(), "{view}");
}

#[tokio::test]
async fn a_tune_is_refused_in_gaming_mode_and_for_a_folder_from_source() {
    let s = setup("tune-refused").await;
    installed(&s);
    let (code, _) = post(&s, "/gaming", json!({"enabled": true})).await;
    assert_eq!(code, 200);
    let (code, answer) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 409, "{answer}");
    assert!(answer.to_string().contains("Gaming mode"), "{answer}");

    let s = setup("tune-source").await;
    let cfg = &s.agent.cfg;
    SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef").model(
        "iq3_xxs",
        json!({"exe": "strata.exe", "args": ["--native", "a.gguf"], "port": 8080}),
    );
    let view = get(&s, "/strata").await;
    let iq3 = model(&view, "IQ3_XXS");
    assert_eq!(iq3["tunable"], false);
    assert!(
        iq3["tune_unavailable"]
            .as_str()
            .unwrap()
            .contains("Strata Terminal"),
        "{iq3}"
    );
    let (code, answer) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 409, "{answer}");
    assert!(s.fake.runs_of("strata.cmd").is_empty());
}

/// Strata activates the preset as soon as the tuner ends; a cancel right
/// after still puts the settings back.
#[tokio::test]
async fn a_cancel_after_the_tuner_answered_still_undoes_its_preset() {
    let s = setup("tune-cancel-late").await;
    installed(&s);
    tuner_says(&s, &[TUNED.trim_end()], Duration::ZERO, true);
    let (code, _) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202);
    job_until(&s, "the tuner's answer", |j| {
        j["lines"]
            .as_array()
            .is_some_and(|l| l.iter().any(|l| l.as_str().unwrap().contains(PRESET)))
    })
    .await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "cancelled", "{done}");
    let runs = s.fake.runs_of("strata.cmd");
    assert_eq!(
        runs[1].args,
        ["presets", "undo", PRESET, "--output", "json"]
    );
    eventually("Strata started again", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
}

/// An undo Strata refuses leaves the tuned settings active: the job says so,
/// and the tune is still kept.
#[tokio::test]
async fn a_refused_undo_fails_the_tune_but_keeps_its_result() {
    let s = setup("tune-undo-refused").await;
    installed(&s);
    tuner_says(&s, &[TUNED.trim_end()], Duration::ZERO, false);
    s.fake.command(
        "strata.cmd presets",
        FakeCommand {
            lines: vec![
                r#"{"ok": false, "error": {"code": "not_found", "message": "No such preset"}}"#
                    .into(),
            ],
            exit: Some(4),
            ..FakeCommand::default()
        },
    );
    let (code, _) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "failed", "{done}");
    let error = done["error"].as_str().unwrap();
    assert!(error.contains("the tuned settings stay active"), "{error}");
    assert!(error.contains("No such preset"), "{error}");
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["last_tune"]["code"], "faster");
}

/// A tuner that does not run reports why, not a missing answer.
#[tokio::test]
async fn a_tuner_that_fails_without_an_answer_gives_its_reason() {
    let s = setup("tune-crash").await;
    installed(&s);
    s.fake.command(
        "strata.cmd tune",
        FakeCommand {
            lines: vec!["Traceback: engine missing".into()],
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    let (code, _) = post(&s, "/strata/jobs", tune("IQ3_XXS")).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_ne!(done["error"], "The tuner gave no answer", "{done}");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}
