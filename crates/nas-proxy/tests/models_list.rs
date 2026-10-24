//! Apps list the models first (`GET /v1/models`, `/models`) and give up after
//! a few seconds: while Strata is not ready they get the saved list at once,
//! and a client that hangs up does not stop the wake-up its request started.

use crate::common;

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{events::EventKind, proxy::STATUS_HEADER};

async fn get(url: &str, key: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{url}{path}"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
}

/// A proxy that saw Strata list `Qwen3-30B-A3B` while the PC was awake,
/// restarted over the same `DATA_DIR` in front of the PC asleep: it has
/// not seen Strata answer since.
async fn with_saved_list(name: &str, tweak: impl FnOnce(&mut strata_nas_proxy::Config)) -> Setup {
    let dir = data_dir(name);
    let s = setup(true, |c| {
        c.data_dir = Some(dir);
        tweak(c);
    })
    .await;
    let resp = get(&s.url, &s.key, "/v1/models").await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get(STATUS_HEADER).is_none());
    s.pc.awake.store(false, Ordering::SeqCst);
    let state = restarted(&s);
    state.spawn_idle_loop();
    Setup {
        url: serve_proxy(state.clone()).await,
        state,
        ..s
    }
}

fn listed(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap()["data"][0]["id"].clone()
}

#[tokio::test]
async fn a_sleeping_pc_answers_the_list_at_once_without_waking() {
    let s = with_saved_list("models-asleep", |_| {}).await;
    let before = own_kinds(&s.state);
    for path in ["/v1/models", "/models?limit=5"] {
        let started = Instant::now();
        let resp = get(&s.url, &s.key, path).await;
        assert!(started.elapsed() < Duration::from_secs(1), "{path}");
        assert_eq!(resp.status(), 200, "{path}");
        assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
        assert_eq!(resp.headers()["content-type"], "application/json");
        assert_eq!(listed(&resp.text().await.unwrap()), "Qwen3-30B-A3B");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert!(!s.pc.up());
    assert_eq!(own_kinds(&s.state), before);
}

#[tokio::test]
async fn the_list_needs_an_api_key() {
    let s = with_saved_list("models-key", |_| {}).await;
    let resp = get(&s.url, "wrong", "/v1/models").await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn other_paths_still_wake_the_pc() {
    let s = with_saved_list("models-other-path", |_| {}).await;
    // Strata has no such path; the wake-up is what counts here.
    get(&s.url, &s.key, "/v1/models/Qwen3-30B-A3B").await;
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    assert!(kinds(&s.state).contains(&EventKind::Awake));
}

#[tokio::test]
async fn gaming_mode_answers_the_list_at_once() {
    let s = with_saved_list("models-gaming", |_| {}).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    s.pc.gaming.store(true, Ordering::SeqCst);
    let resp = get(&s.url, &s.key, "/models").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_awake_strata_answers_the_list_itself() {
    let s = with_saved_list("models-awake", |_| {}).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    *s.pc.listed_model.lock().unwrap() = Some("coder-IQ1_M".into());
    let resp = get(&s.url, &s.key, "/v1/models").await;
    assert!(resp.headers().get(STATUS_HEADER).is_none());
    assert_eq!(listed(&resp.text().await.unwrap()), "coder-IQ1_M");

    // The new list is the copy now.
    s.pc.starting.store(true, Ordering::SeqCst);
    let state = restarted(&s);
    let url = serve_proxy(state).await;
    let resp = get(&url, &s.key, "/v1/models").await;
    assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
    assert_eq!(listed(&resp.text().await.unwrap()), "coder-IQ1_M");
}

#[tokio::test]
async fn a_list_without_models_keeps_the_saved_one() {
    let s = with_saved_list("models-empty", |_| {}).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    s.pc.no_model.store(true, Ordering::SeqCst);
    let resp = get(&s.url, &s.key, "/v1/models").await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"], json!([]));

    s.pc.starting.store(true, Ordering::SeqCst);
    let url = serve_proxy(restarted(&s)).await;
    let resp = get(&url, &s.key, "/v1/models").await;
    assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
    assert_eq!(listed(&resp.text().await.unwrap()), "Qwen3-30B-A3B");
}

#[tokio::test]
async fn listing_models_does_not_keep_the_pc_awake() {
    let s = with_saved_list("models-idle", |c| {
        c.idle_timeout = Duration::from_millis(600)
    })
    .await;
    // Up, but Strata still starting: the list comes from the copy.
    s.pc.awake.store(true, Ordering::SeqCst);
    s.pc.starting.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.pc.sleep_requests.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "listing the models kept the PC awake"
        );
        let resp = get(&s.url, &s.key, "/models").await;
        assert_eq!(resp.headers()[STATUS_HEADER], "models_saved");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_wake_up_saves_the_list_without_any_app_asking() {
    let dir = data_dir("models-after-wake");
    let s = setup(false, |c| c.data_dir = Some(dir)).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 200);
    eventually("the list fetched", || {
        s.pc.models_requests.load(Ordering::SeqCst) == 1
    })
    .await;

    s.pc.awake.store(false, Ordering::SeqCst);
    let url = serve_proxy(restarted(&s)).await;
    eventually_async("the list saved", || async {
        let resp = get(&url, &s.key, "/v1/models").await;
        resp.headers()
            .get(STATUS_HEADER)
            .is_some_and(|v| v == "models_saved")
    })
    .await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 1);
}

/// `eventually` for a check that asks the proxy.
async fn eventually_async<F: Future<Output = bool>>(what: &str, check: impl Fn() -> F) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check().await {
        assert!(Instant::now() < deadline, "{what} did not happen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_first_packet_goes_out_before_the_pc_service_answers() {
    let s = setup(false, |_| {}).await;
    *s.pc.status_delay.lock().unwrap() = Duration::from_millis(1500);
    let (url, key) = (s.url.clone(), s.key.clone());
    let request = tokio::spawn(async move { get(&url, &key, "/v1/models").await.status() });
    let started = Instant::now();
    while s.pc.wol_packets.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "no packet before the PC service answered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(request.await.unwrap(), 200);
    // The packet ahead and the wake-up's own share the resend interval.
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 1);
    let kinds = kinds(&s.state);
    assert_eq!(
        kinds.iter().filter(|k| **k == EventKind::WakeSent).count(),
        1
    );
}

/// A client that gives up on the list after `after`, like an app's timeout.
async fn hang_up(s: &Setup, after: Duration) {
    let resp = reqwest::Client::new()
        .get(format!("{}/v1/models", s.url))
        .bearer_auth(&s.key)
        .timeout(after)
        .send()
        .await;
    assert!(resp.is_err(), "answered before the client hung up");
}

#[tokio::test]
async fn a_client_that_hangs_up_while_the_pc_service_is_asked_still_wakes_the_pc() {
    let s = setup(false, |_| {}).await;
    *s.pc.status_delay.lock().unwrap() = Duration::from_millis(800);
    hang_up(&s, Duration::from_millis(300)).await;
    eventually("the packet sent ahead", || {
        s.pc.wol_packets.load(Ordering::SeqCst) == 1
    })
    .await;
    // Recorded by the wake-up only, once the PC service has answered.
    assert!(!kinds(&s.state).contains(&EventKind::WakeSent));
    seen(&s, EventKind::Awake).await;
    let kinds = kinds(&s.state);
    assert!(kinds.contains(&EventKind::WakeSent), "{kinds:?}");
    assert!(s.pc.up());
}

#[tokio::test]
async fn a_client_that_hangs_up_during_the_wake_up_does_not_end_it() {
    let s = setup(false, |_| {}).await;
    // The PC takes its time: the client is gone long before.
    s.pc.ignore_wol.store(true, Ordering::SeqCst);
    hang_up(&s, Duration::from_millis(300)).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    seen(&s, EventKind::Awake).await;
    let resp = get(&s.url, &s.key, "/v1/models").await;
    assert!(resp.headers().get(STATUS_HEADER).is_none());
    assert_eq!(listed(&resp.text().await.unwrap()), "Qwen3-30B-A3B");
}
