//! API keys: every request to Strata needs a key created on the API keys page.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{AppState, Config, api_router};

/// A request with the given headers and no others.
async fn send(url: &str, path: &str, headers: &[(&str, &str)], body: Value) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("{url}{path}"))
        .json(&body);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    req.send().await.unwrap()
}

async fn create(b: &mut Browser, name: &str) -> reqwest::Response {
    b.call("create-api-key", &[("name", name)]).await
}

async fn list(b: &mut Browser) -> Value {
    let resp = b.call("api-keys", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn request_without_key_is_refused_and_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    let resp = send(&s.url, "/v1/messages", &[], json!({})).await;
    assert_eq!(resp.status(), 401);
    assert_eq!(resp.headers()["x-strata-proxy-status"], "unauthorized");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wrong_key_gets_an_openai_error_for_openai_paths() {
    let s = setup(true, |_| {}).await;
    let resp = send(
        &s.url,
        "/v1/chat/completions",
        &[("authorization", "Bearer shp_falsch")],
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refused_requests_do_not_keep_the_pc_awake() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(400)).await;
    for _ in 0..6 {
        send(
            &s.url,
            "/v1/messages",
            &[("x-api-key", "falsch")],
            json!({}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(s.pc.sleep_requests.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn without_any_key_every_request_is_refused() {
    let state = AppState::new(Config {
        strata_url: Some("http://127.0.0.1:9".into()),
        ..Config::default()
    });
    let url = format!("http://{}", serve(api_router(state)).await);
    let resp = send(&url, "/v1/messages", &[], json!({})).await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn valid_key_works_as_bearer_and_as_x_api_key() {
    let s = setup(true, |_| {}).await;
    let bearer = format!("Bearer {}", s.key);
    let resp = send(
        &s.url,
        "/v1/chat/completions",
        &[("authorization", &bearer)],
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let resp = send(&s.url, "/v1/messages", &[("x-api-key", &s.key)], json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 2);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen, Auth::default());
}

#[tokio::test]
async fn streamed_requests_with_key_still_wake_and_stream() {
    let s = setup(false, |_| {}).await;
    let resp = send(
        &s.url,
        "/v1/messages",
        &[("x-api-key", &s.key)],
        json!({"stream": true}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("message_stop"), "{text}");
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.x_api_key, None);
}

#[tokio::test]
async fn a_created_key_is_shown_once_and_then_works() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = create(&mut b, "Claude Code Laptop").await;
    assert_eq!(resp.status(), 200);
    let created: Value = resp.json().await.unwrap();
    let key = created["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("shp_"), "{key}");
    assert_eq!(created["info"]["name"], "Claude Code Laptop");

    let view = list(&mut b).await;
    assert!(!view.to_string().contains(&key));
    let entry = view["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "Claude Code Laptop")
        .unwrap()
        .clone();
    assert!(key.starts_with(entry["prefix"].as_str().unwrap()));
    assert!(entry["created"].as_u64().unwrap() > 0);

    let resp = send(&s.url, "/v1/messages", &[("x-api-key", &key)], json!({})).await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn names_must_be_unique_and_not_empty() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(create(&mut b, "Open WebUI").await.status(), 200);
    assert_eq!(create(&mut b, "open webui").await.status(), 400);
    assert_eq!(create(&mut b, "   ").await.status(), 400);
    assert_eq!(create(&mut b, &"x".repeat(65)).await.status(), 400);
}

#[tokio::test]
async fn key_management_needs_a_login() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(b.call("api-keys", &[]).await.status(), 401);
    assert_eq!(create(&mut b, "Fremd").await.status(), 401);
    let html = b.page("/admin/api-keys").await;
    assert!(html.contains("Anmelden"), "{html}");
}

#[tokio::test]
async fn keys_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("strata-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = Config {
        data_dir: Some(dir.clone()),
        ..Config::default()
    };
    let first = AppState::new(base.clone());
    let mut b = logged_in(&format!("http://{}", serve(api_router(first)).await)).await;
    let created: Value = create(&mut b, "Laptop").await.json().await.unwrap();
    let key = created["key"].as_str().unwrap().to_string();

    // Without a Strata URL a valid key gets "not configured", a wrong one 401.
    let second = AppState::new(base);
    let url = format!("http://{}", serve(api_router(second)).await);
    let resp = send(&url, "/v1/messages", &[("x-api-key", &key)], json!({})).await;
    assert_eq!(resp.status(), 503);
    let resp = send(&url, "/v1/messages", &[("x-api-key", "falsch")], json!({})).await;
    assert_eq!(resp.status(), 401);
    std::fs::remove_dir_all(&dir).unwrap();
}
