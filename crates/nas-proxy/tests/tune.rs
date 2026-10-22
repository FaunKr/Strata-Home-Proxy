//! A tune keeps Strata off on the PC for up to two hours: the proxy answers
//! at once instead of waking, and holds the idle time until the tune ends.

use crate::common;

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{events::EventKind, proxy::STATUS_HEADER};
use strata_pc_protocol::{StrataJobKind, StrataJobState};

/// The PC as it is while a tune runs: the service answers, Strata does not.
fn tuning(s: &Setup) {
    *s.pc.strata_job.lock().unwrap() = Some(job(StrataJobKind::Tune, StrataJobState::Running));
    s.pc.starting.store(true, Ordering::SeqCst);
}

#[tokio::test]
async fn requests_get_an_answer_at_once_while_tuning() {
    let s = setup(true, |_| {}).await;
    tuning(&s);
    let started = Instant::now();
    let resp = post_json(&s, "/v1/messages", json!({"model": "x"})).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()[STATUS_HEADER], "tuning");
    assert!(resp.headers().contains_key("retry-after"));
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("Strata is tuning"), "{message}");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_pc_stays_awake_until_the_tune_ends() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(400)).await;
    tuning(&s);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);

    s.pc.update_job(|j| j.state = StrataJobState::Done);
    s.pc.starting.store(false, Ordering::SeqCst);
    eventually("a sleep request", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn a_tune_started_from_the_page_reaches_the_pc_service() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[
                ("kind", "tune"),
                ("model", "IQ3_XXS"),
                ("tune_mode", "thorough"),
                ("tune_use", "chat"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body = s.pc.job_bodies.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        body,
        json!({"kind": "tune", "model": "IQ3_XXS", "mode": "thorough", "use": "chat"})
    );
    eventually("the started event", || {
        own_kinds(&s.state).contains(&EventKind::StrataJobStarted)
    })
    .await;
}
