//! What the service keeps in every model config: loop recovery and the
//! recommended sampling (#157), at its start, after install and after an update.

mod common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, strata};

fn write(path: PathBuf, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn config_path(s: &Setup, id: &str) -> PathBuf {
    s.agent.cfg.strata_dir.join(format!("strata-{id}.json"))
}

fn read_config(s: &Setup, id: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(config_path(s, id)).unwrap()).unwrap()
}

/// Strata with IQ3_XXS (active) and qwen36-Q8_0, as setup writes them.
fn installed(s: &Setup) {
    let strata = s.agent.cfg.strata_dir.clone();
    write(strata.join("setup.py"), "");
    write(strata.join(".git").join("HEAD"), "0123456789abcdef\n");
    for (id, arch) in [("iq3_xxs", "qwen4exp"), ("qwen36-q8_0", "qwen35moe")] {
        write(
            strata.join(format!("strata-{id}.json")),
            &json!({"exe": "C:/Strata/engine/strata.exe", "args": ["--native", "a.gguf"],
                "port": 8080, "api_key": "key-0123456789abcdef", "arch": arch})
            .to_string(),
        );
        write(strata.join(format!("run-{id}.bat")), "");
    }
    s.agent
        .set_managed(Managed {
            strata_start: Some(strata.join("run-iq3_xxs.bat")),
            strata_args: vec![],
            strata_processes: Some(vec![format!("{}/", strata.display())]),
            strata_api_key: Some("key-0123456789abcdef".into()),
        })
        .unwrap();
}

fn modified(s: &Setup, id: &str) -> std::time::SystemTime {
    std::fs::metadata(config_path(s, id))
        .unwrap()
        .modified()
        .unwrap()
}

// --- #158 loop recovery ---

#[tokio::test]
async fn at_start_every_model_config_gets_loop_recovery_and_keeps_the_rest() {
    let s = setup("configs-start").await;
    installed(&s);
    let before = modified(&s, "iq3_xxs");
    tokio::time::sleep(Duration::from_millis(20)).await;
    strata::manage_configs_at_start(&s.agent).await;
    for id in ["iq3_xxs", "qwen36-q8_0"] {
        let cfg = read_config(&s, id);
        assert_eq!(cfg["reasoning_loop_recovery"], "recover", "{id}: {cfg}");
        assert_eq!(cfg["args"], json!(["--native", "a.gguf"]), "{id}");
        assert_eq!(cfg["port"], 8080, "{id}");
        assert_eq!(cfg["api_key"], "key-0123456789abcdef", "{id}");
    }
    assert_eq!(
        modified(&s, "iq3_xxs"),
        before,
        "the active model stays the one touched last"
    );
    let text = std::fs::read_to_string(config_path(&s, "iq3_xxs")).unwrap();
    assert!(text.starts_with("{\n \""), "Strata's indent of one: {text}");
}

#[tokio::test]
async fn at_start_strata_restarts_after_its_request_and_only_on_a_change() {
    let s = setup("configs-start-restart").await;
    installed(&s);
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let agent = s.agent.clone();
    let start = tokio::spawn(async move { strata::manage_configs_at_start(&agent).await });
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        0,
        "stopped mid-request"
    );
    s.fake.strata_busy.store(false, Ordering::SeqCst);
    start.await.unwrap();
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);

    strata::manage_configs_at_start(&s.restarted()).await;
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        1,
        "nothing changed, no restart"
    );
}

#[tokio::test]
async fn a_config_that_cannot_be_read_is_left_and_the_others_change() {
    let s = setup("configs-broken").await;
    installed(&s);
    write(config_path(&s, "qwen36-q8_0"), "{ kaputt");
    strata::manage_configs_at_start(&s.agent).await;
    assert_eq!(
        read_config(&s, "iq3_xxs")["reasoning_loop_recovery"],
        "recover"
    );
    assert_eq!(
        std::fs::read_to_string(config_path(&s, "qwen36-q8_0")).unwrap(),
        "{ kaputt"
    );
}

#[tokio::test]
async fn an_update_sets_loop_recovery_before_strata_starts() {
    let s = setup("configs-update").await;
    installed(&s);
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "update"})).await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    assert_eq!(
        read_config(&s, "qwen36-q8_0")["reasoning_loop_recovery"],
        "recover"
    );
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        1,
        "one start, with the new config"
    );
}

#[tokio::test]
async fn a_model_added_gets_loop_recovery() {
    let s = setup("configs-add").await;
    installed(&s);
    let strata = s.agent.cfg.strata_dir.clone();
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            creates: vec![(
                strata.join("strata-swift-iq2_xs.json"),
                json!({"exe": "strata.exe", "args": []}).to_string(),
            )],
            ..FakeCommand::default()
        },
    );
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    assert_eq!(
        read_config(&s, "swift-iq2_xs")["reasoning_loop_recovery"],
        "recover"
    );
    assert_eq!(
        read_config(&s, "iq3_xxs")["reasoning_loop_recovery"],
        "recover"
    );
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        1,
        "the active config changed too"
    );
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} fehlt: {view}"))
}

#[tokio::test]
async fn the_page_shows_each_models_loop_recovery() {
    let s = setup("configs-view-loop").await;
    installed(&s);
    let mut cfg = read_config(&s, "qwen36-q8_0");
    cfg["reasoning_loop_recovery"] = json!("stop");
    write(config_path(&s, "qwen36-q8_0"), &cfg.to_string());
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["loop_recovery"], "off", "{view}");
    assert_eq!(
        model(&view, "qwen36-Q8_0")["loop_recovery"],
        "stop",
        "{view}"
    );
    strata::manage_configs_at_start(&s.agent).await;
    let view = get(&s, "/strata").await;
    assert_eq!(
        model(&view, "IQ3_XXS")["loop_recovery"],
        "recover",
        "{view}"
    );
}
