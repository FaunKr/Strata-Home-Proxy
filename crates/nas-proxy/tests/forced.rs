//! Forced sleep and forced Strata restart from the web UI. Neither wakes
//! the PC.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::events::EventKind;

/// The detail of the latest event of `kind`.
fn detail(s: &Setup, kind: EventKind) -> String {
    s.state
        .events
        .recent()
        .into_iter()
        .rev()
        .find(|e| e.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {:?}", kinds(&s.state)))
        .detail
        .unwrap_or_default()
}

#[tokio::test]
async fn forced_sleep_follows_a_refused_sleep_and_the_pc_counts_as_asleep() {
    let s = setup(true, |_| {}).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    let refused: Value = b.call("sleep-now", &[]).await.json().await.unwrap();
    assert!(refused["Refused"]["reason"].is_string(), "{refused}");

    let resp = b.call("force-sleep", &[]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(s.pc.force_sleeps.load(Ordering::SeqCst), 1);
    assert!(!s.pc.up());
    // Like an accepted sleep request: settling, then asleep.
    let v = live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["sleep"], "Asleep");
    let kinds = own_kinds(&s.state);
    assert!(
        kinds.ends_with(&[EventKind::ForcedSleep, EventKind::SleepAccepted]),
        "{kinds:?}"
    );
    assert_eq!(
        detail(&s, EventKind::ForcedSleep),
        "from the web UI, overriding user activity"
    );
    // The next request wakes it as after any sleep.
    post_json(&s, "/v1/messages", json!({})).await;
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn forced_sleep_names_gaming_mode_and_the_cancelled_job() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    *s.pc.strata_job.lock().unwrap() = Some(job(StrataJobKind::AddModel, StrataJobState::Running));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Gaming", |v| v["pc"] == "Gaming").await;

    assert_eq!(b.call("force-sleep", &[]).await.status(), 200);
    assert_eq!(
        detail(&s, EventKind::ForcedSleep),
        "from the web UI, overriding gaming mode, cancelled job: Loading model"
    );
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
}

#[tokio::test]
async fn forced_restart_reaches_the_pc_service_and_strata_loads() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_job.lock().unwrap() = Some(job(StrataJobKind::Activate, StrataJobState::Running));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;

    let resp = b.call("strata-force-restart", &[]).await;
    assert_eq!(resp.status(), 200);
    let answer: Value = resp.json().await.unwrap();
    assert_eq!(
        answer,
        json!({"cancelled_job": "activate", "started": true})
    );
    assert_eq!(s.pc.strata_kills.load(Ordering::SeqCst), 1);
    assert_eq!(
        detail(&s, EventKind::ForcedStrataRestart),
        "from the web UI, cancelled job: Making model active, Strata started again"
    );
    live.until("Strata loading", |v| v["strata"] == "Loading")
        .await;
    // The PC stays awake.
    assert!(s.pc.up());
    assert!(!kinds(&s.state).contains(&EventKind::SleepAccepted));
}

#[tokio::test]
async fn forced_restart_is_refused_in_gaming_mode() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let resp = b.call("strata-force-restart", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("Gaming mode is on"));
    assert!(!kinds(&s.state).contains(&EventKind::ForcedStrataRestart));
}

#[tokio::test]
async fn neither_wakes_a_sleeping_pc() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    for name in ["force-sleep", "strata-force-restart"] {
        let resp = b.call(name, &[]).await;
        assert_eq!(resp.status(), 400, "{name}");
        assert!(resp.text().await.unwrap().contains("asleep"), "{name}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.force_sleeps.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.strata_kills.load(Ordering::SeqCst), 0);
    assert!(!s.pc.up());
}

#[tokio::test]
async fn an_older_pc_service_asks_for_an_update() {
    let s = setup(true, |_| {}).await;
    s.pc.no_force.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    for name in ["force-sleep", "strata-force-restart"] {
        let resp = b.call(name, &[]).await;
        assert_eq!(resp.status(), 400, "{name}");
        let text = resp.text().await.unwrap();
        assert!(
            text.ends_with("The PC service is too old for this, update it in the PC section."),
            "{name}: {text}"
        );
    }
    assert!(s.pc.up());
    let kinds = kinds(&s.state);
    assert!(!kinds.contains(&EventKind::ForcedSleep), "{kinds:?}");
    assert!(!kinds.contains(&EventKind::SleepAccepted), "{kinds:?}");
}

#[tokio::test]
async fn a_pc_service_that_does_not_answer_says_so() {
    // Strata answers, so the PC counts as awake; its service does not.
    let s = setup(true, |c| c.pc_agent_url = Some("http://127.0.0.1:9".into())).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    for name in ["force-sleep", "strata-force-restart"] {
        let resp = b.call(name, &[]).await;
        assert_eq!(resp.status(), 400, "{name}");
        let text = resp.text().await.unwrap();
        assert!(
            text.ends_with("The PC service does not answer."),
            "{name}: {text}"
        );
    }
}

#[tokio::test]
async fn both_need_a_pc_service() {
    let s = setup(true, |c| c.pc_agent_url = None).await;
    let mut b = logged_in(&s.url).await;
    for name in ["force-sleep", "strata-force-restart"] {
        let resp = b.call(name, &[]).await;
        assert_eq!(resp.status(), 400, "{name}");
        assert!(
            resp.text().await.unwrap().contains("PC_AGENT_URL"),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_forced_restart_past_the_proxys_limit_counts_as_no_answer() {
    let s = setup(true, |c| c.force_timeout = Duration::from_millis(500)).await;
    *s.pc.kill_delay.lock().unwrap() = Duration::from_secs(3);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    let started = std::time::Instant::now();
    let resp = b.call("strata-force-restart", &[]).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(resp.status(), 400);
    let text = resp.text().await.unwrap();
    assert!(text.ends_with("The PC service does not answer."), "{text}");
    assert!(!kinds(&s.state).contains(&EventKind::ForcedStrataRestart));
}
