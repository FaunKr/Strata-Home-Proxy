//! The public model name: apps of an API key that hides the model see one
//! name instead of the model Strata runs, in the list, in every answer and
//! while the PC sleeps; their requests reach the running model.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{api_keys::ModelName, proxy::STATUS_HEADER};

const REAL: &str = "Qwen3-30B-A3B";

async fn hiding() -> Setup {
    setup(true, |c| c.hide_model = true).await
}

async fn get(s: &Setup, key: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", s.url))
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
}

fn ids(list: &Value) -> Vec<&str> {
    list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect()
}

/// The `model` Strata got with the last request to `path`.
fn sent_model(s: &Setup, path: &str) -> Value {
    s.pc.strata_bodies
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(p, _)| p == path)
        .map(|(_, body)| body["model"].clone())
        .unwrap_or_else(|| panic!("no request to {path}"))
}

fn key(s: &Setup, name: &str, model_name: ModelName) -> String {
    s.state.api_keys.create(name, model_name).unwrap().0
}

#[tokio::test]
async fn the_switch_off_shows_the_real_names() {
    let s = setup(true, |_| {}).await;
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let list: Value = get(&s, &s.key, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL, "qwen", "flux-schnell"]);
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "strata"})).await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], REAL);
    assert_eq!(sent_model(&s, "/v1/chat/completions"), "strata");
}

#[tokio::test]
async fn the_list_shows_one_chat_model_under_the_public_name() {
    let s = hiding().await;
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let resp = get(&s, &s.key, "/v1/models").await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(!text.contains(REAL), "{text}");
    let list: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(ids(&list), ["strata", "flux-schnell"]);
    let chat = &list["data"][0];
    assert_eq!(chat["meta"]["n_ctx"], 32768);
    assert_eq!(
        chat["architecture"]["input_modalities"],
        json!(["text", "image"])
    );
    assert!(chat.get("aliases").is_none(), "{chat}");
    assert!(chat.get("alias_of").is_none(), "{chat}");
}

#[tokio::test]
async fn the_public_name_is_the_one_set() {
    let s = hiding().await;
    s.state
        .settings
        .set("PUBLIC_MODEL_NAME", "local-llm")
        .unwrap();
    let list: Value = get(&s, &s.key, "/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["local-llm"]);
}

#[tokio::test]
async fn a_request_reaches_the_running_model_whatever_name_it_sends() {
    let s = hiding().await;
    for name in ["strata", "coder-IQ1_M", "qwen"] {
        let resp = post_json(&s, "/v1/chat/completions", json!({"model": name})).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(sent_model(&s, "/v1/chat/completions"), REAL, "{name}");
    }
}

#[tokio::test]
async fn a_request_keeps_its_name_while_health_names_no_model() {
    let s = setup(false, |c| c.hide_model = true).await;
    s.pc.health_without_model.store(true, Ordering::SeqCst);
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "strata"})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(sent_model(&s, "/v1/chat/completions"), "strata");
    // The answer still names the public name.
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "strata");
}

#[tokio::test]
async fn an_answer_names_the_public_name() {
    let s = hiding().await;
    s.state
        .settings
        .set("PUBLIC_MODEL_NAME", "local-llm")
        .unwrap();
    let chat: Value = post_json(&s, "/v1/chat/completions", json!({"model": "x"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(chat["model"], "local-llm");
    assert_eq!(chat["choices"][0]["message"]["content"], "Hallo");
    let message: Value = post_json(&s, "/v1/messages", json!({"model": "x"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(message["model"], "local-llm");
}

#[tokio::test]
async fn streamed_answers_name_the_public_name() {
    let s = hiding().await;
    for path in ["/v1/chat/completions", "/v1/messages", "/v1/responses"] {
        let resp = post_json(&s, path, json!({"model": "x", "stream": true})).await;
        assert_eq!(resp.status(), 200, "{path}");
        let text = resp.text().await.unwrap();
        assert!(!text.contains(REAL), "{path}: {text}");
        assert!(text.contains("\"model\":\"strata\""), "{path}: {text}");
    }
}

#[tokio::test]
async fn a_stream_during_a_wake_up_names_the_public_name() {
    let s = setup(false, |c| c.hide_model = true).await;
    let resp = post_json(
        &s,
        "/v1/chat/completions",
        json!({"model": "x", "stream": true}),
    )
    .await;
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hallo"), "{text}");
    assert!(!text.contains(REAL), "{text}");
    assert!(text.contains("\"model\":\"strata\""), "{text}");
    assert_eq!(sent_model(&s, "/v1/chat/completions"), REAL);
}

#[tokio::test]
async fn the_image_model_keeps_its_name() {
    let s = hiding().await;
    let resp = post_json(
        &s,
        "/v1/images/generations",
        json!({"model": "flux-schnell"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "flux-schnell");
    assert_eq!(sent_model(&s, "/v1/images/generations"), "flux-schnell");
}

#[tokio::test]
async fn a_key_set_to_show_sees_the_real_names_under_the_switch() {
    let s = hiding().await;
    let mine = key(&s, "My script", ModelName::Show);
    let list: Value = get(&s, &mine, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL]);
    let body: Value = post_json_to(&s.url, &mine, "/v1/chat/completions", json!({"model": "x"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["model"], REAL);
    assert_eq!(sent_model(&s, "/v1/chat/completions"), "x");
}

#[tokio::test]
async fn a_key_set_to_hide_sees_the_public_name_without_the_switch() {
    let s = setup(true, |_| {}).await;
    let app = key(&s, "Chat app", ModelName::Hide);
    let list: Value = get(&s, &app, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["strata"]);
    // The other key still sees the real names.
    let list: Value = get(&s, &s.key, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL]);
}

#[tokio::test]
async fn the_saved_list_has_the_public_name_while_the_pc_sleeps() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("public-name-asleep"))).await;
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let list: Value = get(&s, &s.key, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL, "qwen", "flux-schnell"]);
    list_saved_on_disk(&s, "flux-schnell").await;
    s.pc.awake.store(false, Ordering::SeqCst);
    s.pc.starting.store(true, Ordering::SeqCst);
    let state = restarted(&s);
    let url = serve_proxy(state.clone()).await;
    let asleep = Setup { url, state, ..s };

    // The switch counts at once, also while the PC sleeps.
    asleep.state.settings.set("HIDE_MODEL", "true").unwrap();
    let resp = get(&asleep, &asleep.key, "/v1/models").await;
    assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
    let list: Value = resp.json().await.unwrap();
    assert_eq!(ids(&list), ["strata", "flux-schnell"]);

    asleep.state.settings.set("HIDE_MODEL", "false").unwrap();
    let list: Value = get(&asleep, &asleep.key, "/v1/models")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&list), [REAL, "qwen", "flux-schnell"]);
    assert_eq!(asleep.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn errors_pass_unchanged() {
    let s = hiding().await;
    s.pc.chat_fails.store(true, Ordering::SeqCst);
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 500);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], REAL);
    assert_eq!(body["error"]["message"], "Qwen3-30B-A3B failed");
}

#[tokio::test]
async fn relay_mode_hides_the_model_too() {
    let s = setup(true, |c| {
        c.hide_model = true;
        c.pc_agent_token = Some("pc-token".into());
        c.idle_check = std::time::Duration::from_secs(3600);
    })
    .await;
    s.pc.relay.store(true, Ordering::SeqCst);
    s.pc.set_strata_key("strata-key");
    s.state.ask_pc(&s.state.agent()).await.unwrap();
    let body: Value = post_json(&s, "/v1/chat/completions", json!({"model": "x"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["model"], "strata");
    assert_eq!(sent_model(&s, "/v1/chat/completions"), REAL);
    assert_eq!(s.pc.direct_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_settings_page_refuses_a_name_apps_cannot_use() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let too_long = "x".repeat(65);
    for value in ["", "my model", "tab\tname", too_long.as_str()] {
        let resp = save_settings(&mut b, &[("PUBLIC_MODEL_NAME", value)]).await;
        assert_eq!(resp.status(), 400, "{value:?}");
    }
    let resp = save_settings(&mut b, &[("HIDE_MODEL", "maybe")]).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(s.state.cfg().public_model_name, "strata");
    assert!(!s.state.cfg().hide_model);

    let resp = save_settings(
        &mut b,
        &[("HIDE_MODEL", "true"), ("PUBLIC_MODEL_NAME", "local-llm")],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let list: Value = get(&s, &s.key, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["local-llm"]);
}

#[tokio::test]
async fn a_key_is_created_with_its_model_name_setting_and_changed_later() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("public-name-keys"))).await;
    let mut b = logged_in(&s.url).await;
    let created: Value = b
        .call(
            "create-api-key",
            &[("name", "Chat app"), ("model_name", "hide")],
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(created["info"]["model_name"], "hide");
    let app = created["key"].as_str().unwrap().to_string();
    let prefix = created["info"]["prefix"].as_str().unwrap().to_string();
    let list: Value = get(&s, &app, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["strata"]);

    let resp = b
        .call(
            "set-api-key-model-name",
            &[
                ("name", "Chat app"),
                ("prefix", &prefix),
                ("model_name", "show"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let list: Value = get(&s, &app, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL]);

    // The setting survives a restart.
    let state = restarted(&s);
    let shown = state
        .api_keys
        .list()
        .into_iter()
        .find(|k| k.name == "Chat app")
        .unwrap();
    assert_eq!(shown.model_name, ModelName::Show);
}

#[tokio::test]
async fn a_key_created_without_a_choice_follows_the_switch() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let created: Value = b
        .call("create-api-key", &[("name", "Laptop")])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(created["info"]["model_name"], "global");
    let laptop = created["key"].as_str().unwrap().to_string();
    let list: Value = get(&s, &laptop, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL]);
    s.state.settings.set("HIDE_MODEL", "true").unwrap();
    let list: Value = get(&s, &laptop, "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["strata"]);
}

#[tokio::test]
async fn setting_the_model_name_of_an_unknown_key_is_refused() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "set-api-key-model-name",
            &[
                ("name", "Nobody"),
                ("prefix", "shp_00000000"),
                ("model_name", "show"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn a_key_file_from_before_the_setting_follows_the_switch() {
    let dir = data_dir("public-name-old-keys");
    std::fs::create_dir_all(&dir).unwrap();
    // A key file as proxies before the setting wrote it: the key `shp_old`.
    let file = json!({"keys": [{"name": "Old", "prefix": "shp_old",
        "hash": sha256_hex("shp_old"), "created": 1}]});
    std::fs::write(dir.join("api-keys.json"), file.to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir)).await;
    let list: Value = get(&s, "shp_old", "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), [REAL]);
    s.state.settings.set("HIDE_MODEL", "true").unwrap();
    let list: Value = get(&s, "shp_old", "/v1/models").await.json().await.unwrap();
    assert_eq!(ids(&list), ["strata"]);
}

fn sha256_hex(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
