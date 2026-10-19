//! Der Verlauf, so wie die Seite ihn sieht: der Proxy zeichnet, was er beim
//! Blick auf den PC bemerkt, und die Geschichte zählt nur, was er wirklich
//! beobachtet hat.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::Value;
use strata_nas_proxy::{clock, events::EventKind};

/// Die Geschichte, die die Seite für die letzten `days` Tage bekäme.
async fn history(b: &mut Browser, days: u32) -> Value {
    let resp = b.call("history", &[("days", &days.to_string())]).await;
    assert_eq!(resp.status(), 200, "history({days})");
    resp.json().await.unwrap()
}

/// Der heutige Tag; die Liste kommt vom ältesten zum neuesten.
fn today(view: &Value) -> &Value {
    &view["days"][view["days"].as_array().unwrap().len() - 1]
}

/// Die Stunden eines Tages: wach, gaming, schlafend, unbekannt.
fn hours(day: &Value) -> (f64, f64, f64, f64) {
    let part = |key: &str| day[key].as_f64().unwrap_or_default();
    (
        part("awake_h"),
        part("gaming_h"),
        part("asleep_h"),
        part("unknown_h"),
    )
}

#[tokio::test]
async fn a_pc_woken_by_hand_counts_as_awake() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    // Der Proxy sieht den schlafenden PC, ohne etwas dazu getan zu haben.
    seen(&s, EventKind::PcSeenAsleep).await;
    // Jemand weckt den PC am Tisch.
    s.pc.awake.store(true, Ordering::SeqCst);
    seen(&s, EventKind::PcSeenAwake).await;
    let view = history(&mut b, 7).await;
    let (awake, _, asleep, _) = hours(today(&view));
    assert!(asleep > 0.0, "die schlafende Zeit fehlt: {view}");
    assert!(awake > 0.0, "die geweckte Zeit fehlt: {view}");
}

#[tokio::test]
async fn a_pc_put_to_sleep_by_hand_counts_as_sleep() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    seen(&s, EventKind::PcSeenAwake).await;
    // Jemand schickt den PC von Hand schlafen.
    s.pc.awake.store(false, Ordering::SeqCst);
    seen(&s, EventKind::PcSeenAsleep).await;
    let view = history(&mut b, 7).await;
    let (awake, _, asleep, _) = hours(today(&view));
    assert!(awake > 0.0, "die wache Zeit vorher fehlt: {view}");
    assert!(asleep > 0.0, "die schlafende Zeit fehlt: {view}");
}

#[tokio::test]
async fn gaming_mode_counts_as_gaming() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    seen(&s, EventKind::PcSeenAwake).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    seen(&s, EventKind::PcSeenGaming).await;
    let view = history(&mut b, 7).await;
    let (_, gaming, _, _) = hours(today(&view));
    assert!(gaming > 0.0, "die Gaming-Zeit fehlt: {view}");
}

#[tokio::test]
async fn a_missed_answer_while_strata_answers_makes_no_sleep() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    seen(&s, EventKind::PcSeenAwake).await;
    // Der PC-Dienst antwortet viele Male nicht, obwohl der PC läuft.
    s.pc.status_misses.store(200, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !kinds(&s.state).contains(&EventKind::PcSeenAsleep),
        "ein verpasster Antwortversuch wird zu schlafend"
    );
    let view = history(&mut b, 7).await;
    let (_, _, asleep, _) = hours(today(&view));
    assert_eq!(asleep, 0.0, "die Geschichte erfindet Schlaf");
}

#[tokio::test]
async fn a_restart_leaves_unknown_time_until_the_next_known_state() {
    let dir = std::env::temp_dir().join(format!("strata-history-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Die Ereignisse eines früheren Laufs: der PC schlief, bevor der Proxy
    // neu started. Wo er aufwachte, hat niemand aufgezeichnet.
    let ago = clock::unix_now() - 400;
    std::fs::write(
        dir.join("events.jsonl"),
        format!("{{\"ts\":{ago},\"kind\":\"pc_seen_asleep\"}}\n"),
    )
    .unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut b = logged_in(&s.url).await;
    seen(&s, EventKind::PcSeenAwake).await;
    let view = history(&mut b, 7).await;
    let (awake, _, asleep, unknown) = hours(today(&view));
    assert!(unknown > 0.0, "die Lücke über den Neustart fehlt: {view}");
    // Der Schlaf vor dem Neustart zählt nicht weiter: der Proxy kann nicht
    // wissen, wann er endete.
    assert_eq!(asleep, 0.0, "schlafend über den Neustart hinüber: {view}");
    assert!(awake > 0.0, "die wache Zeit nach dem Neustart fehlt");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_fresh_install_explains_the_empty_history() {
    // Der Leerlauf-Prüfer kommt nie dazu: der Verlauf bleibt leer.
    let s = setup(true, |c| c.idle_check = Duration::from_secs(3600)).await;
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/pc/history").await;
    assert!(html.contains("Awake and asleep"), "{html}");
    assert!(html.contains("Nothing to show yet"), "{html}");
    assert!(html.contains("Last 7 days"), "{html}");
}

#[tokio::test]
async fn the_history_page_names_the_pc_service_it_needs() {
    let s = setup(true, |c| {
        c.pc_agent_url = None;
        c.idle_check = Duration::from_secs(3600);
    })
    .await;
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/pc/history").await;
    assert!(html.contains("Without a PC service"), "{html}");
}
