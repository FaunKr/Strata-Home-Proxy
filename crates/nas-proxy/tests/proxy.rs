//! End-to-end tests against a simulated PC: Strata, the PC service and a Wake
//! on LAN listener, all on localhost.

mod common;

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{events::EventKind, proxy::STATUS_HEADER, state::PcState};

#[tokio::test]
async fn awake_pc_is_passed_through() {
    let s = setup(true, |_| {}).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hallo");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sleeping_pc_is_woken_for_plain_request() {
    let s = setup(false, |_| {}).await;
    let resp = post_json(&s, "/v1/messages", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "Hallo");
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    let kinds = kinds(&s.state);
    assert!(kinds.contains(&EventKind::WakeSent));
    assert!(kinds.contains(&EventKind::Awake));
}

#[tokio::test]
async fn sleeping_pc_streamed_request_gets_pings_then_answer() {
    let s = setup(false, |c| c.ping_interval = Duration::from_millis(100)).await;
    let resp = post_json(&s, "/v1/messages", json!({"model": "x", "stream": true})).await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.unwrap();
    assert!(text.starts_with("event: ping\n"), "{text}");
    let start = text.find("message_start").unwrap();
    assert!(text[..start].matches("event: ping").count() >= 2, "{text}");
    assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
}

#[tokio::test]
async fn openai_stream_uses_comment_pings_while_waking() {
    let s = setup(false, |c| c.ping_interval = Duration::from_millis(100)).await;
    let resp = post_json(&s, "/v1/chat/completions", json!({"stream": true})).await;
    let text = resp.text().await.unwrap();
    assert!(text.starts_with(": ping\n\n"), "{text}");
    assert!(!text.contains("event: ping"));
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn gaming_mode_answers_at_once() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let resp = post_json(&s, "/v1/messages", json!({"model": "x"})).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()[STATUS_HEADER], "gaming");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.state.pc_state(), PcState::Gaming);
}

#[tokio::test]
async fn wake_timeout_is_reported() {
    let s = setup(false, |c| c.wake_timeout = Duration::from_millis(800)).await;
    s.pc.ignore_wol.store(true, Ordering::SeqCst);
    let resp = post_json(&s, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 504);
    assert_eq!(resp.headers()[STATUS_HEADER], "wake_timeout");
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn idle_pc_is_sent_to_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(400)).await;
    post_json(&s, "/v1/messages", json!({})).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert!(!s.pc.up());
    assert_eq!(s.state.pc_state(), PcState::Asleep);

    // The next request wakes it again.
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn running_stream_keeps_pc_awake() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(300)).await;
    let client = s.clone();
    let stream = tokio::spawn(async move {
        post_json(
            &client,
            "/v1/messages",
            json!({"stream": true, "pause_ms": 1200}),
        )
        .await
        .text()
        .await
        .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(s.state.in_flight(), 1);
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    let text = stream.await.unwrap();
    assert!(text.contains("message_stop"));
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

/// A setup whose Strata metrics show nothing running, which gives up a
/// request in flight after 300 ms.
async fn quiet_strata() -> Setup {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(200)).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    s.state.set_lost_after(Duration::from_millis(300));
    s
}

#[tokio::test]
async fn lost_stream_is_given_up_and_pc_sleeps() {
    let s = quiet_strata().await;
    let started = Instant::now();
    let resp = post_json(
        &s,
        "/v1/messages",
        json!({"stream": true, "pause_ms": 30000}),
    )
    .await;
    let text = resp.text().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{text}");
    assert!(text.contains("message_start"), "{text}");
    assert!(text.contains("event: error"), "{text}");
    assert!(!text.contains("message_stop"), "{text}");
    eventually("no request in flight", || s.state.in_flight() == 0).await;
    eventually("the PC is sent to sleep", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn lost_plain_request_is_given_up() {
    let s = quiet_strata().await;
    *s.pc.chat_pause.lock().unwrap() = Duration::from_secs(30);
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 504);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "api_error");
    eventually("no request in flight", || s.state.in_flight() == 0).await;
}

#[tokio::test]
async fn later_request_gets_its_own_time() {
    let s = quiet_strata().await;
    s.state.set_lost_after(Duration::from_millis(800));
    let lost = |s: &Setup| {
        let s = s.clone();
        tokio::spawn(async move {
            let body = json!({"stream": true, "pause_ms": 30000});
            post_json(&s, "/v1/messages", body)
                .await
                .text()
                .await
                .unwrap()
        })
    };
    let first = lost(&s);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let second = lost(&s);
    let text = first.await.unwrap();
    assert!(text.contains("event: error"), "{text}");
    assert_eq!(s.state.in_flight(), 1, "the later request is kept");
    let text = second.await.unwrap();
    assert!(text.contains("event: error"), "{text}");
}

#[tokio::test]
async fn lost_stream_after_a_wake_is_given_up() {
    let s = setup(false, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    s.state.set_lost_after(Duration::from_millis(300));
    let resp = post_json(
        &s,
        "/v1/messages",
        json!({"stream": true, "pause_ms": 30000}),
    )
    .await;
    let text = resp.text().await.unwrap();
    assert!(text.contains("message_start"), "{text}");
    assert!(text.contains("event: error"), "{text}");
    eventually("no request in flight", || s.state.in_flight() == 0).await;
}

#[tokio::test]
async fn request_strata_shows_is_kept() {
    let s = quiet_strata().await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    *s.pc.chat_pause.lock().unwrap() = Duration::from_millis(1200);
    let resp = post_json(&s, "/v1/chat/completions", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hallo");
}

#[tokio::test]
async fn pings_fill_long_pauses_between_events() {
    let s = setup(true, |c| c.ping_interval = Duration::from_millis(150)).await;
    let resp = post_json(&s, "/v1/messages", json!({"stream": true, "pause_ms": 700})).await;
    let text = resp.text().await.unwrap();
    let start = text.find("message_start").unwrap();
    let delta = text.find("content_block_delta").unwrap();
    assert!(
        text[start..delta].matches("event: ping").count() >= 2,
        "{text}"
    );
    // Every event stays whole: split on blank lines, each part is one event.
    for part in text.split("\n\n").filter(|p| !p.is_empty()) {
        assert!(part.starts_with("event: "), "{part:?}");
        assert_eq!(part.matches("data: ").count(), 1, "{part:?}");
    }
}

#[tokio::test]
async fn refused_sleep_is_retried_later() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(200)).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert!(kinds(&s.state).contains(&EventKind::SleepRefused));
    s.pc.refuse_sleep.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 2);
    assert!(!s.pc.up());
}

#[tokio::test]
async fn keep_awake_blocks_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(200)).await;
    s.state.keep_awake_for(Duration::from_millis(800));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_requests_share_one_wake_up() {
    let s = setup(false, |c| c.wol_resend = Duration::from_secs(5)).await;
    let calls = (0..5).map(|_| post_json(&s, "/v1/messages", json!({})));
    for resp in futures_util::future::join_all(calls).await {
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 5);
}
