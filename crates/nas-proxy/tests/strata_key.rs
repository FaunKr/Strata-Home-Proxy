//! The Strata key: the proxy sends its own key to Strata, never the client's.

mod common;

use common::*;
use serde_json::json;

fn bearer(key: &str) -> Option<String> {
    Some(format!("Bearer {key}"))
}

#[tokio::test]
async fn strata_gets_the_strata_key_instead_of_the_clients() {
    let s = setup(true, |_| {}).await;
    s.state
        .settings
        .set("STRATA_API_KEY", "strata-geheim")
        .unwrap();
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("strata-geheim"));
    assert_eq!(seen.x_api_key, None);
}

#[tokio::test]
async fn health_checks_carry_the_strata_key() {
    let s = setup(false, |_| {}).await;
    s.state
        .settings
        .set("STRATA_API_KEY", "strata-geheim")
        .unwrap();
    post_json(&s, "/v1/chat/completions", json!({})).await;
    let seen = s.pc.health_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("strata-geheim"));
}

#[tokio::test]
async fn without_strata_key_no_key_reaches_strata() {
    let s = setup(true, |_| {}).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen, SeenCredentials::default());
}

#[tokio::test]
async fn strata_key_is_shown_only_as_set() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(&mut b, &[("STRATA_API_KEY", "strata-geheim")]).await;
    assert_eq!(resp.status(), 200);
    let text = b.call("settings", &[]).await.text().await.unwrap();
    assert!(!text.contains("strata-geheim"), "{text}");
    let view: serde_json::Value = serde_json::from_str(&text).unwrap();
    let entry = view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "STRATA_API_KEY")
        .unwrap()
        .clone();
    assert_eq!(entry["value"], "set");
    assert_eq!(entry["secret"], true);
}

#[tokio::test]
async fn every_call_of_the_strata_client_carries_the_strata_key() {
    let s = setup(true, |_| {}).await;
    s.state
        .settings
        .set("STRATA_API_KEY", "strata-geheim")
        .unwrap();
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
        bearer("strata-geheim")
    );
    strata.metrics().await.unwrap();
    assert_eq!(
        s.pc.metrics_auth.lock().unwrap().clone(),
        bearer("strata-geheim")
    );
    strata
        .chat(&json!({"messages": []}), std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        s.pc.api_auth.lock().unwrap().take().unwrap().authorization,
        bearer("strata-geheim")
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
        bearer("strata-geheim")
    );
}

#[tokio::test]
async fn without_a_strata_address_there_is_no_strata_client() {
    let s = setup(true, |c| c.strata_url = None).await;
    assert!(s.state.strata().is_none());
}
