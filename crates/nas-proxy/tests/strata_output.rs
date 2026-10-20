//! The page "Strata output": Strata's console output from the PC service,
//! live and only while the page is open, without ever waking the PC.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;

const STREAM: &str = "/admin/api/strata-output-stream";

#[tokio::test]
async fn output_from_the_pc_reaches_the_page() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_log.lock().unwrap() = vec![
        StrataOutputMessage::Started { at: 1_760_000_000 },
        StrataOutputMessage::Line {
            text: "Loading model".into(),
        },
        StrataOutputMessage::Notice {
            reason: StrataOutputNotice::Running,
        },
    ];
    let b = logged_in(&s.url).await;
    // Nobody looks yet: the proxy leaves the PC service alone.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.strata_log_requests.load(Ordering::SeqCst), 0);

    let mut live = Live::open_at(&b, STREAM).await;
    live.until("reset", |m| m["kind"] == "reset").await;
    let started = live.until("separator line", |m| m["kind"] == "line").await;
    assert!(
        started["text"]
            .as_str()
            .unwrap()
            .starts_with("Strata restarted, "),
        "{started}"
    );
    live.until("line", |m| m["text"] == "Loading model").await;
    live.until("notice", |m| m["notice"] == "running").await;

    s.pc.strata_log
        .lock()
        .unwrap()
        .push(StrataOutputMessage::Replace {
            text: "Model loaded".into(),
        });
    live.until("replace", |m| {
        m["kind"] == "replace" && m["text"] == "Model loaded"
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
    live.until("notice", |m| m["notice"] == "asleep").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_page_follows_a_pc_that_wakes_up() {
    let s = setup(false, |_| {}).await;
    *s.pc.strata_log.lock().unwrap() = vec![StrataOutputMessage::Line {
        text: "after the wake-up".into(),
    }];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("notice", |m| m["notice"] == "asleep").await;
    s.pc.awake.store(true, Ordering::SeqCst);
    live.until("reset", |m| m["kind"] == "reset").await;
    live.until("line", |m| m["text"] == "after the wake-up")
        .await;
}

#[tokio::test]
async fn gaming_mode_reaches_the_page_as_a_notice() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    *s.pc.strata_log.lock().unwrap() = vec![StrataOutputMessage::Notice {
        reason: StrataOutputNotice::Gaming,
    }];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("notice", |m| m["notice"] == "gaming").await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_open_page_does_not_keep_the_pc_awake() {
    // Long enough that the page sees the PC awake first, also on a slow runner.
    let s = setup(true, |c| c.idle_timeout = Duration::from_secs(2)).await;
    *s.pc.strata_log.lock().unwrap() = vec![StrataOutputMessage::Line {
        text: "running".into(),
    }];
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("line", |m| m["text"] == "running").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while s.pc.sleep_requests.load(Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no sleep request");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_refusing_pc_service_is_not_called_asleep() {
    let s = setup(true, |_| {}).await;
    s.pc.refuse_log.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("notice", |m| m["notice"] == "refused").await;
}
