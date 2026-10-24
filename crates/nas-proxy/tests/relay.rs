//! Relay mode: Strata listens on the PC only, and the proxy reaches it
//! through the PC service's relay, which adds the Strata key.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;

const TOKEN: &str = "pc-token";

fn bearer(secret: &str) -> Option<String> {
    Some(format!("Bearer {secret}"))
}

/// A PC whose Strata runs in relay mode, with `key`; the proxy has seen
/// its status once. Only the test asks for the status.
async fn relayed(awake: bool) -> Setup {
    let s = setup(true, |c| {
        c.pc_agent_token = Some(TOKEN.into());
        c.idle_check = Duration::from_secs(3600);
    })
    .await;
    s.pc.relay.store(true, Ordering::SeqCst);
    s.pc.set_strata_key("strata-key");
    s.state.ask_pc(&s.state.agent()).await.unwrap();
    s.pc.awake.store(awake, Ordering::SeqCst);
    s
}

fn no_direct_request(s: &Setup) {
    assert_eq!(s.pc.direct_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_client_request_goes_through_the_relay_with_the_pc_service_token() {
    let s = relayed(true).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["choices"][0]["message"]["content"],
        "Hallo"
    );
    assert_eq!(*s.pc.relay_auth.lock().unwrap(), bearer(TOKEN));
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("strata-key"), "the relay's key");
    assert_eq!(seen.x_api_key, None);
    no_direct_request(&s);
}

#[tokio::test]
async fn a_stream_goes_through_the_relay() {
    let s = relayed(true).await;
    let resp = post_json(&s, "/v1/messages", json!({"stream": true, "pause_ms": 300})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hallo"), "{text}");
    assert!(text.contains("message_stop"), "{text}");
    no_direct_request(&s);
}

#[tokio::test]
async fn health_and_metrics_go_through_the_relay() {
    let s = relayed(true).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("busy", |v| v["strata"]["Busy"].is_object())
        .await;
    assert_eq!(
        v["strata"]["Busy"],
        json!({"processing": 1, "deferred": 1, "tokens_per_sec": 38.5})
    );
    assert_eq!(
        s.pc.metrics_auth.lock().unwrap().as_deref(),
        Some("Bearer strata-key")
    );
    let health = s.pc.health_auth.lock().unwrap().clone().unwrap();
    assert_eq!(health.authorization, bearer("strata-key"));
    no_direct_request(&s);
}

#[tokio::test]
async fn the_proxy_fetches_no_strata_key_in_relay_mode() {
    let s = relayed(true).await;
    s.pc.set_strata_key("next-key");
    s.state.ask_pc(&s.state.agent()).await.unwrap();
    assert_eq!(s.pc.key_requests.load(Ordering::SeqCst), 0);
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, bearer("next-key"));
    assert_eq!(s.pc.key_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_strata_that_does_not_answer_the_pc_service_is_loading() {
    let s = relayed(true).await;
    s.pc.starting.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("loading", |v| v["strata"] == "Loading").await;
    s.pc.starting.store(false, Ordering::SeqCst);
    live.until("ready", |v| v["strata"] == "Ready").await;
    no_direct_request(&s);
}

#[tokio::test]
async fn a_request_waits_for_a_loading_strata_behind_the_relay() {
    let s = relayed(true).await;
    s.pc.starting.store(true, Ordering::SeqCst);
    let pc = s.pc.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        pc.starting.store(false, Ordering::SeqCst);
    });
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0, "the PC is up");
    no_direct_request(&s);
}

#[tokio::test]
async fn a_relay_that_does_not_answer_is_never_unreachable() {
    let s = relayed(true).await;
    s.pc.relay_down.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("loading", |v| v["strata"] == "Loading").await;
    assert_ne!(v["strata"], "Unreachable");
    s.pc.relay_down.store(false, Ordering::SeqCst);
    live.until("ready", |v| v["strata"] == "Ready").await;
}

#[tokio::test]
async fn a_sleeping_pc_is_woken_and_answers_through_the_relay() {
    let s = relayed(false).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    no_direct_request(&s);
}
