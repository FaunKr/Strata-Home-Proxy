//! What the service keeps in every model config: loop recovery and the
//! recommended sampling (#157), at its start, after install and after an update.

mod common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{install::testing::SetupLeaves, managed::Managed, strata};

fn write(path: PathBuf, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// The service's Strata-Ordner, to add to or look at.
fn strata_dir(s: &Setup) -> SetupLeaves {
    SetupLeaves::at(&s.agent.cfg.strata_dir, &s.agent.cfg.strata_data_dir)
}

fn read_config(s: &Setup, id: &str) -> Value {
    let model = strata_dir(s).installation().model(id).unwrap();
    Value::Object(model.fields().clone())
}

/// Strata with IQ3_XXS (active) and qwen36-Q8_0, as setup writes them.
fn installed(s: &Setup) {
    let cfg = &s.agent.cfg;
    let setup = SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    for (id, arch) in [("iq3_xxs", "qwen4exp"), ("qwen36-q8_0", "qwen35moe")] {
        setup.model(
            id,
            json!({"exe": "C:/Strata/engine/strata.exe", "args": ["--native", "a.gguf"],
                "port": 8080, "api_key": "key-0123456789abcdef", "arch": arch}),
        );
    }
    s.agent
        .set_managed(Managed {
            strata_start: Some(setup.installation().run_script("iq3_xxs")),
            strata_args: vec![],
            strata_processes: Some(vec![format!("{}/", cfg.strata_dir.display())]),
            strata_api_key: Some("key-0123456789abcdef".into()),
        })
        .unwrap();
}

fn modified(s: &Setup, id: &str) -> std::time::SystemTime {
    strata_dir(s).modified(id)
}

// --- #158 loop recovery ---

#[tokio::test]
async fn at_start_every_model_config_gets_loop_recovery_and_keeps_the_rest() {
    let s = setup("configs-start").await;
    installed(&s);
    let before = modified(&s, "iq3_xxs");
    tokio::time::sleep(Duration::from_millis(20)).await;
    strata::manage_configs_at_start(&s.agent).await.await;
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
    let text = strata_dir(&s).text("iq3_xxs");
    assert!(text.starts_with("{\n \""), "Strata's indent of one: {text}");
}

#[tokio::test]
async fn at_start_strata_restarts_after_its_request_and_only_on_a_change() {
    let s = setup("configs-start-restart").await;
    installed(&s);
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let agent = s.agent.clone();
    let start = tokio::spawn(async move { strata::manage_configs_at_start(&agent).await.await });
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

    strata::manage_configs_at_start(&s.restarted()).await.await;
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
    strata_dir(&s).config_text("qwen36-q8_0", "{ kaputt");
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(
        read_config(&s, "iq3_xxs")["reasoning_loop_recovery"],
        "recover"
    );
    assert_eq!(strata_dir(&s).text("qwen36-q8_0"), "{ kaputt");
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
    let config = strata_dir(&s).installation().config_path("swift-iq2_xs");
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            creates: vec![(config, json!({"exe": "strata.exe", "args": []}).to_string())],
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
    strata_dir(&s).config("qwen36-q8_0", cfg);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["loop_recovery"], "off", "{view}");
    assert_eq!(
        model(&view, "qwen36-Q8_0")["loop_recovery"],
        "stop",
        "{view}"
    );
    strata::manage_configs_at_start(&s.agent).await.await;
    let view = get(&s, "/strata").await;
    assert_eq!(
        model(&view, "IQ3_XXS")["loop_recovery"],
        "recover",
        "{view}"
    );
}

// --- #159 recommended sampling by Strata's version ---

/// Strata's `serve/server.py`, with the names the service looks for.
fn strata_knows(s: &Setup, config_block: bool, model_card: bool) {
    let mut text = String::from("import os\n");
    if config_block {
        text.push_str("def sampling_defaults_from_config(cfg: dict) -> dict:\n    ...\n");
    }
    if model_card {
        text.push_str("MODEL_CARD_SAMPLING = {}\n");
    }
    write(
        s.agent.cfg.strata_dir.join("serve").join("server.py"),
        &text,
    );
}

fn set_sampling(s: &Setup, id: &str, block: Value) {
    let mut cfg = read_config(s, id);
    cfg["sampling"] = block;
    strata_dir(s).config(id, cfg);
}

#[tokio::test]
async fn a_strata_without_model_card_sampling_gets_the_thinking_set_in_the_config() {
    let s = setup("configs-sampling-block").await;
    installed(&s);
    strata_knows(&s, true, false);
    set_sampling(&s, "iq3_xxs", json!({"temperature": 0.2, "seed": 7}));
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(
        read_config(&s, "iq3_xxs")["sampling"],
        json!({"temperature": 1.0, "top_p": 0.95, "top_k": 20})
    );
    assert_eq!(
        read_config(&s, "qwen36-q8_0")["sampling"],
        json!({"temperature": 1.0, "top_p": 0.95, "top_k": 20, "presence_penalty": 1.5})
    );
}

#[tokio::test]
async fn a_strata_with_model_card_sampling_gets_no_block_in_the_config() {
    let s = setup("configs-sampling-card").await;
    installed(&s);
    strata_knows(&s, true, true);
    set_sampling(
        &s,
        "iq3_xxs",
        json!({"temperature": 1.0, "top_p": 0.95, "top_k": 20}),
    );
    set_sampling(&s, "qwen36-q8_0", json!({"temperature": 0.2}));
    strata::manage_configs_at_start(&s.agent).await.await;
    for id in ["iq3_xxs", "qwen36-q8_0"] {
        let cfg = read_config(&s, id);
        assert!(cfg.get("sampling").is_none(), "{id}: {cfg}");
        assert_eq!(cfg["reasoning_loop_recovery"], "recover", "{id}");
    }
}

#[tokio::test]
async fn a_strata_that_reads_no_sampling_gets_no_block() {
    let s = setup("configs-sampling-old").await;
    installed(&s);
    strata_knows(&s, false, false);
    strata::manage_configs_at_start(&s.agent).await.await;
    assert!(read_config(&s, "iq3_xxs").get("sampling").is_none());
}

#[tokio::test]
async fn an_update_to_model_card_sampling_takes_the_block_out() {
    let s = setup("configs-sampling-update").await;
    installed(&s);
    strata_knows(&s, true, false);
    strata::manage_configs_at_start(&s.agent).await.await;
    assert!(read_config(&s, "iq3_xxs")["sampling"].is_object());
    let server = s.agent.cfg.strata_dir.join("serve").join("server.py");
    std::fs::remove_file(&server).unwrap();
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            creates: vec![(
                server,
                "def sampling_defaults_from_config(cfg):\nMODEL_CARD_SAMPLING = {}\n".into(),
            )],
            ..FakeCommand::default()
        },
    );
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "update"})).await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    assert!(read_config(&s, "iq3_xxs").get("sampling").is_none());
}

fn field(set: &Value, name: &str) -> (f64, String) {
    let f = set
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == name)
        .unwrap_or_else(|| panic!("{name} fehlt: {set}"));
    (
        f["value"].as_f64().unwrap(),
        f["source"].as_str().unwrap().into(),
    )
}

#[tokio::test]
async fn the_page_shows_the_sampling_each_model_gets_and_where_it_comes_from() {
    let s = setup("configs-view-sampling").await;
    installed(&s);
    strata_knows(&s, true, true);
    set_sampling(
        &s,
        "qwen36-q8_0",
        json!({"temperature": 0.6, "repeat_penalty": 1.1}),
    );
    let view = get(&s, "/strata").await;
    let qwen = &model(&view, "IQ3_XXS")["sampling"];
    assert_eq!(qwen["support"], "model_card", "{view}");
    assert_eq!(
        field(&qwen["thinking"], "temperature"),
        (1.0, "model_card".into())
    );
    assert_eq!(
        field(&qwen["instruct"], "presence_penalty"),
        (1.5, "model_card".into())
    );
    let names: Vec<&str> = qwen["instruct"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["temperature", "top_p", "top_k", "presence_penalty"]);
    let q36 = &model(&view, "qwen36-Q8_0")["sampling"];
    assert_eq!(
        field(&q36["thinking"], "temperature"),
        (0.6, "config".into())
    );
    assert_eq!(
        field(&q36["thinking"], "presence_penalty"),
        (1.5, "model_card".into())
    );
    assert_eq!(
        field(&q36["instruct"], "repetition_penalty"),
        (1.1, "config".into())
    );
}

#[tokio::test]
async fn an_unreadable_strata_version_leaves_the_block() {
    let s = setup("configs-sampling-unknown").await;
    installed(&s);
    set_sampling(&s, "iq3_xxs", json!({"temperature": 0.2}));
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(
        read_config(&s, "iq3_xxs")["sampling"],
        json!({"temperature": 0.2})
    );
    assert_eq!(
        read_config(&s, "iq3_xxs")["reasoning_loop_recovery"],
        "recover"
    );
    let view = get(&s, "/strata").await;
    assert!(model(&view, "IQ3_XXS")["sampling"].is_null(), "{view}");
}

#[tokio::test]
async fn a_strata_that_reads_no_sampling_shows_none() {
    let s = setup("configs-view-old").await;
    installed(&s);
    strata_knows(&s, false, false);
    let view = get(&s, "/strata").await;
    let sampling = &model(&view, "IQ3_XXS")["sampling"];
    assert_eq!(sampling["support"], "none", "{view}");
    assert_eq!(sampling["thinking"], json!([]));
}

// --- #160 Chat settings over the recommended sampling ---

#[tokio::test]
async fn chat_settings_show_as_overriding_and_stay_untouched() {
    let s = setup("configs-chat").await;
    installed(&s);
    strata_knows(&s, true, true);
    let chat = r#"{"temperature": 0.3, "reasoning_effort": "high", "max_tokens": 4096}"#;
    strata_dir(&s)
        .chat_settings("iq3_xxs", chat)
        .chat_settings("qwen36-q8_0", "{ kaputt");
    strata::manage_configs_at_start(&s.agent).await.await;
    let view = get(&s, "/strata").await;
    let qwen = &model(&view, "IQ3_XXS")["sampling"];
    for set in ["thinking", "instruct"] {
        assert_eq!(
            field(&qwen[set], "temperature"),
            (0.3, "chat_settings".into()),
            "{set}"
        );
        assert_eq!(
            field(&qwen[set], "top_k"),
            (20.0, "model_card".into()),
            "{set}"
        );
    }
    let names = qwen["thinking"].to_string();
    assert!(
        !names.contains("max_tokens") && !names.contains("reasoning_effort"),
        "{names}"
    );
    let q36 = &model(&view, "qwen36-Q8_0")["sampling"];
    assert_eq!(
        field(&q36["thinking"], "temperature"),
        (1.0, "model_card".into())
    );
    assert_eq!(strata_dir(&s).chat_text("iq3_xxs"), chat);
}
