//! The Strata key (#217): the PC service makes a new one at every start of
//! Strata, the proxy fetches it and sends it to Strata, never the client's.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::json;
use strata_nas_proxy::events::EventKind;

fn bearer(key: &str) -> Option<String> {
    Some(format!("Bearer {key}"))
}

/// The proxy asks the PC service for its status, as its loops do.
async fn noted(s: &Setup) {
    s.state.ask_pc(&s.state.agent()).await.unwrap();
}

#[tokio::test]
async fn strata_gets_the_key_of_the_pc_service_instead_of_the_clients() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("first-key");
    noted(&s).await;
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("first-key"));
    assert_eq!(seen.x_api_key, None);
}

#[tokio::test]
async fn a_new_generation_brings_the_new_key() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("first-key");
    noted(&s).await;
    noted(&s).await;
    assert_eq!(
        s.pc.key_requests.load(Ordering::SeqCst),
        1,
        "same generation"
    );
    assert!(!kinds(&s.state).contains(&EventKind::StrataKeyChanged));

    s.pc.set_strata_key("second-key");
    noted(&s).await;
    assert_eq!(s.pc.key_requests.load(Ordering::SeqCst), 2);
    assert!(kinds(&s.state).contains(&EventKind::StrataKeyChanged));
    s.state.strata().unwrap().health().await.unwrap();
    let seen = s.pc.health_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("second-key"));
}

#[tokio::test]
async fn a_refused_key_is_fetched_again_and_the_request_sent_once_more() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("first-key");
    noted(&s).await;
    // Strata started again with a new key the proxy has not seen yet.
    s.pc.set_strata_key("second-key");
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.key_requests.load(Ordering::SeqCst), 2);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("second-key"));
}

#[tokio::test]
async fn right_after_a_start_of_the_proxy_a_refused_request_fetches_the_first_key() {
    let s = setup(true, |_| {}).await;
    // The proxy has not asked the PC yet and holds no key.
    s.pc.set_strata_key("first-key");
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    assert!(!kinds(&s.state).contains(&EventKind::StrataKeyChanged));
}

#[tokio::test]
async fn a_key_strata_refuses_still_reaches_the_client_as_its_refusal() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("right-key");
    *s.pc.reported_key.lock().unwrap() = Some("wrong-key".into());
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 401);
    assert_eq!(
        resp.headers()["x-strata-proxy-status"],
        "strata_unauthorized"
    );
    // One fetch for the 401, one for the second 401 with the same key; no loop.
    assert!(s.pc.key_requests.load(Ordering::SeqCst) <= 2);

    *s.pc.reported_key.lock().unwrap() = None;
    assert_eq!(post_json(&s, "/v1/messages", json!({})).await.status(), 200);
}

#[tokio::test]
async fn without_a_key_from_the_pc_service_no_key_reaches_strata() {
    let s = setup(true, |_| {}).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen, SeenCredentials::default());
}

#[tokio::test]
async fn an_older_pc_service_is_named_when_strata_wants_a_key() {
    let s = setup(false, |_| {}).await;
    s.pc.no_key_endpoint.store(true, Ordering::SeqCst);
    *s.pc.strata_key.lock().unwrap() = Some("right-key".into());
    let resp = post_json(&s, "/v1/messages", json!({"stream": true})).await;
    // The status line went out before Strata answered; the error comes as an event.
    let text = resp.text().await.unwrap();
    assert!(text.contains("update the PC service"), "{text}");
}

#[tokio::test]
async fn the_strata_page_tells_when_the_pc_service_reports_no_key() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    s.pc.no_key_endpoint.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains("too old for the Strata key"), "{html}");

    s.pc.no_key_endpoint.store(false, Ordering::SeqCst);
    s.pc.set_strata_key("first-key");
    let html = b.page("/admin/strata/connection").await;
    assert!(!html.contains("too old for the Strata key"), "{html}");
    assert!(!html.contains("first-key"), "{html}");
}

#[tokio::test]
async fn every_call_of_the_strata_client_carries_the_strata_key() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("strata-secret");
    noted(&s).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    let strata = s.state.strata().unwrap();

    strata.health().await.unwrap();
    assert_eq!(
        s.pc.health_auth
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .authorization,
        bearer("strata-secret")
    );
    strata.metrics().await.unwrap();
    assert_eq!(
        s.pc.metrics_auth.lock().unwrap().clone(),
        bearer("strata-secret")
    );
    strata
        .chat(&json!({"messages": []}), std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        s.pc.api_auth.lock().unwrap().take().unwrap().authorization,
        bearer("strata-secret")
    );
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("authorization", "Bearer client".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    let resp = strata
        .forward(
            reqwest::Method::POST,
            "/v1/messages",
            headers,
            json!({}).to_string().into(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        s.pc.api_auth.lock().unwrap().take().unwrap().authorization,
        bearer("strata-secret")
    );
}

#[tokio::test]
async fn the_key_stays_in_memory_and_a_saved_one_is_deleted_at_start() {
    let dir = data_dir("strata-key-memory");
    let data = dir.clone();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    s.state.settings.set("PC_MAC", "AA:BB:CC:01:02:03").unwrap();
    // As an older proxy saved it.
    let file = dir.join("settings.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    saved["values"]["STRATA_API_KEY"] = json!("old-saved-key");
    std::fs::write(&file, saved.to_string()).unwrap();

    s.pc.set_strata_key("fetched-key");
    noted(&s).await;
    let restarted = restarted(&s);
    assert_eq!(restarted.strata_key.key(), None, "memory only");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("STRATA_API_KEY"), "{text}");
    assert!(text.contains("PC_MAC"), "other settings stay: {text}");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("fetched-key"),
            "{}",
            path.display()
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
