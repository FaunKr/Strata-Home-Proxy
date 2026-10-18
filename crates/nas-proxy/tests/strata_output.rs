//! The page "Strata-Ausgabe": Strata's console output from the PC service,
//! live and only while the page is open, without ever waking the PC.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;

const STREAM: &str = "/admin/api/strata-output-stream";

#[tokio::test]
async fn output_from_the_pc_reaches_the_page() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_log.lock().unwrap() = vec![
        json!({"kind": "started", "at": 1_760_000_000u64}),
        json!({"kind": "line", "text": "Lade Modell"}),
        json!({"kind": "notice", "reason": "running"}),
    ];
    let b = logged_in(&s.url).await;
    // Nobody looks yet: the proxy leaves the PC service alone.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.strata_log_requests.load(Ordering::SeqCst), 0);

    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Neuanfang", |m| m["kind"] == "reset").await;
    let started = live.until("Trennzeile", |m| m["kind"] == "line").await;
    assert!(
        started["text"]
            .as_str()
            .unwrap()
            .starts_with("Strata neu gestartet, "),
        "{started}"
    );
    live.until("Zeile", |m| m["text"] == "Lade Modell").await;
    live.until("Hinweis", |m| m["notice"] == "running").await;

    s.pc.strata_log
        .lock()
        .unwrap()
        .push(json!({"kind": "replace", "text": "Modell geladen"}));
    live.until("Ersetzen", |m| {
        m["kind"] == "replace" && m["text"] == "Modell geladen"
    })
    .await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_output_needs_a_login() {
    let s = setup(true, |_| {}).await;
    let resp = Live::request_at(&Browser::new(&s.url), STREAM).await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn a_sleeping_pc_is_a_notice_and_stays_asleep() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Hinweis", |m| m["notice"] == "asleep").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_page_follows_a_pc_that_wakes_up() {
    let s = setup(false, |_| {}).await;
    *s.pc.strata_log.lock().unwrap() = vec![json!({"kind": "line", "text": "nach dem Aufwachen"})];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Hinweis", |m| m["notice"] == "asleep").await;
    s.pc.awake.store(true, Ordering::SeqCst);
    live.until("Neuanfang", |m| m["kind"] == "reset").await;
    live.until("Zeile", |m| m["text"] == "nach dem Aufwachen")
        .await;
}

#[tokio::test]
async fn gaming_mode_reaches_the_page_as_a_notice() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    *s.pc.strata_log.lock().unwrap() = vec![json!({"kind": "notice", "reason": "gaming"})];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Hinweis", |m| m["notice"] == "gaming").await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_open_page_does_not_keep_the_pc_awake() {
    // Long enough that the page sees the PC awake first, also on a slow runner.
    let s = setup(true, |c| c.idle_timeout = Duration::from_secs(2)).await;
    *s.pc.strata_log.lock().unwrap() = vec![json!({"kind": "line", "text": "läuft"})];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Zeile", |m| m["text"] == "läuft").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while s.pc.sleep_requests.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "keine Schlafen-Anfrage"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_refusing_pc_service_is_not_called_asleep() {
    let s = setup(true, |_| {}).await;
    s.pc.refuse_log.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("Hinweis", |m| m["notice"] == "refused").await;
}
