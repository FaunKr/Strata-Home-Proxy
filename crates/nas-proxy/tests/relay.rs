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

/// A PC whose Strata runs in relay mode, with its Strata key; the proxy
/// has seen its status once. Only the test asks for the status.
async fn relayed(awake: bool) -> Setup {
    relayed_in(awake, None).await
}

async fn relayed_in(awake: bool, data_dir: Option<std::path::PathBuf>) -> Setup {
    let s = setup(true, |c| {
        c.pc_agent_token = Some(TOKEN.into());
        c.idle_check = Duration::from_secs(3600);
        c.data_dir = data_dir;
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

#[tokio::test]
async fn after_a_restart_of_the_proxy_the_first_request_takes_the_relay() {
    let s = relayed_in(true, Some(data_dir("relay-restart"))).await;
    let state = restarted(&s);
    let url = serve_proxy(state).await;
    // Before any status: the request itself goes the way the proxy kept.
    let resp = post_json_to(&url, &s.key, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    no_direct_request(&s);
}

#[tokio::test]
async fn the_update_summary_s_chat_goes_through_the_relay() {
    let s = relayed(true).await;
    let answer = s
        .state
        .strata()
        .unwrap()
        .chat(&json!({"messages": []}), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(answer.content, "Hallo");
    assert!(s.pc.relay_requests.load(Ordering::SeqCst) >= 1);
    no_direct_request(&s);
}

/// The PC answers with an installed Strata; the proxy reaches Strata
/// directly.
async fn switchable() -> (Setup, Browser) {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let b = logged_in(&s.url).await;
    (s, b)
}

async fn switch(b: &mut Browser, on: bool) -> reqwest::Response {
    b.call("strata-relay", &[("on", if on { "true" } else { "false" })])
        .await
}

async fn relay_view(b: &mut Browser) -> serde_json::Value {
    let resp = b.call("strata", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json::<serde_json::Value>().await.unwrap()["relay"].clone()
}

async fn refused(resp: reqwest::Response, why: &str) {
    assert_eq!(resp.status(), 400);
    let text = resp.text().await.unwrap();
    assert!(text.contains(why), "{text}");
}

#[tokio::test]
async fn the_switch_starts_the_job_on_the_pc_and_notes_it_in_the_history() {
    let (s, mut b) = switchable().await;
    assert_eq!(
        relay_view(&mut b).await,
        json!({"on": false, "switching": false, "blocked": null})
    );

    assert_eq!(switch(&mut b, true).await.status(), 200);
    assert_eq!(
        s.pc.job_bodies.lock().unwrap().last().cloned().unwrap(),
        json!({"kind": "set_relay", "on": true})
    );
    seen(&s, strata_nas_proxy::events::EventKind::RelayOn).await;
    assert_eq!(relay_view(&mut b).await["switching"], true);

    s.pc.update_job(|j| j.state = strata_pc_protocol::StrataJobState::Done);
    s.pc.relay.store(true, Ordering::SeqCst);
    assert_eq!(
        relay_view(&mut b).await,
        json!({"on": true, "switching": false, "blocked": null})
    );
    assert_eq!(switch(&mut b, false).await.status(), 200);
    seen(&s, strata_nas_proxy::events::EventKind::RelayOff).await;
}

#[tokio::test]
async fn requests_keep_the_old_path_until_strata_runs_in_the_new_mode() {
    let (s, mut b) = switchable().await;
    switch(&mut b, true).await;
    assert_eq!(
        post_json(&s, "/v1/chat/completions", json!({}))
            .await
            .status(),
        200
    );
    assert_eq!(
        s.pc.relay_requests.load(Ordering::SeqCst),
        0,
        "Strata straight"
    );

    s.pc.relay.store(true, Ordering::SeqCst);
    s.state.ask_pc(&s.state.agent()).await.unwrap();
    assert_eq!(
        post_json(&s, "/v1/chat/completions", json!({}))
            .await
            .status(),
        200,
        "only the relay reaches Strata now"
    );
    no_direct_request(&s);
}

#[tokio::test]
async fn a_sleeping_pc_is_not_woken_for_the_switch() {
    let (s, mut b) = switchable().await;
    s.pc.awake.store(false, Ordering::SeqCst);
    refused(switch(&mut b, true).await, "asleep").await;
    let view = relay_view(&mut b).await;
    assert!(
        view["blocked"].as_str().unwrap().contains("asleep"),
        "{view}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert!(s.pc.job_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_pc_service_there_is_no_switch() {
    let s = setup(true, |c| c.pc_agent_url = None).await;
    let mut b = logged_in(&s.url).await;
    refused(switch(&mut b, true).await, "PC_AGENT_URL").await;
    let view = relay_view(&mut b).await;
    assert!(
        view["blocked"].as_str().unwrap().contains("PC_AGENT_URL"),
        "{view}"
    );
}

#[tokio::test]
async fn the_switch_says_why_it_cannot_be_used() {
    let (s, mut b) = switchable().await;
    s.pc.no_relay.store(true, Ordering::SeqCst);
    refused(switch(&mut b, true).await, "too old").await;
    assert!(
        relay_view(&mut b).await["blocked"]
            .as_str()
            .unwrap()
            .contains("too old")
    );
    s.pc.no_relay.store(false, Ordering::SeqCst);

    s.pc.gaming.store(true, Ordering::SeqCst);
    refused(switch(&mut b, true).await, "Gaming mode").await;
    assert!(
        relay_view(&mut b).await["blocked"]
            .as_str()
            .unwrap()
            .contains("Gaming mode")
    );
    s.pc.gaming.store(false, Ordering::SeqCst);

    s.pc.strata_info
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .info
        .install = Some(strata_pc_protocol::StrataInstall::Source);
    assert!(
        relay_view(&mut b).await["blocked"]
            .as_str()
            .unwrap()
            .contains("old Strata folder")
    );
    assert!(s.pc.job_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_pc_service_s_refusal_comes_back() {
    let (s, mut b) = switchable().await;
    *s.pc.refuse_job.lock().unwrap() = Some("The PC service is updating".into());
    refused(switch(&mut b, true).await, "The PC service is updating").await;
    assert!(
        !kinds(&s.state).contains(&strata_nas_proxy::events::EventKind::RelayOn),
        "no history event for a refused switch"
    );
}
