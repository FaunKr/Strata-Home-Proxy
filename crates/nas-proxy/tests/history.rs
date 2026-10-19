//! Der Verlauf, so wie die Seite ihn sieht: der Proxy zeichnet, was er beim
//! Blick auf den PC bemerkt, und die Geschichte zählt nur, was er wirklich
//! beobachtet hat.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::Value;
use strata_nas_proxy::{clock, events::EventKind};

/// Die Geschichte, die die Seite für diesen Bereich bekäme.
async fn fetch(b: &mut Browser, range: &str) -> Value {
    let resp = b.call("history", &[("range", range)]).await;
    assert_eq!(resp.status(), 200, "history({range})");
    resp.json().await.unwrap()
}

/// Dasselbe, nachdem das letzte Ereignis eine Dauer hat.
async fn history(b: &mut Browser, range: &str) -> Value {
    // Die Geschichte zählt in ganzen Sekunden: ohne diese Pause liegt das
    // letzte Ereignis noch in der Sekunde von "jetzt" und hat keine Dauer.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    fetch(b, range).await
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
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // Jemand weckt den PC am Tisch.
    s.pc.awake.store(true, Ordering::SeqCst);
    seen(&s, EventKind::PcSeenAwake).await;
    let view = history(&mut b, "7d").await;
    let (awake, _, asleep, _) = hours(today(&view));
    assert!(asleep > 0.0, "die schlafende Zeit fehlt: {view}");
    assert!(awake > 0.0, "die geweckte Zeit fehlt: {view}");
}

#[tokio::test]
async fn a_pc_put_to_sleep_by_hand_counts_as_sleep() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    seen(&s, EventKind::PcSeenAwake).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // Jemand schickt den PC von Hand schlafen.
    s.pc.awake.store(false, Ordering::SeqCst);
    seen(&s, EventKind::PcSeenAsleep).await;
    let view = history(&mut b, "7d").await;
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
    let view = history(&mut b, "7d").await;
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
    let view = history(&mut b, "7d").await;
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
    let view = history(&mut b, "7d").await;
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

#[tokio::test]
async fn longer_ranges_come_back_in_weeks_and_months() {
    let dir = std::env::temp_dir().join(format!("strata-history-long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Ein früherer Lauf: der PC schlief vor 400 Tagen ein, wachte vor 100
    // Tagen auf, und der Proxy lief bis vor 10 Tagen.
    let day = 24 * 3600;
    let now = clock::unix_now();
    std::fs::write(
        dir.join("events.jsonl"),
        format!(
            "{{\"ts\":{},\"kind\":\"pc_seen_asleep\"}}\n\
             {{\"ts\":{},\"kind\":\"pc_seen_awake\"}}\n\
             {{\"ts\":{},\"kind\":\"proxy_stopped\"}}\n",
            now - 400 * day,
            now - 100 * day,
            now - 10 * day,
        ),
    )
    .unwrap();
    let s = setup(true, |c| {
        c.data_dir = Some(dir.clone());
        c.idle_check = Duration::from_secs(3600);
    })
    .await;
    let mut b = logged_in(&s.url).await;

    let month = fetch(&mut b, "30d").await;
    assert_eq!(month["bucket"], "day");
    assert_eq!(month["days"].as_array().unwrap().len(), 30);

    let year = fetch(&mut b, "12m").await;
    assert_eq!(year["bucket"], "week", "{year}");
    let weeks = year["days"].as_array().unwrap();
    assert!((53..=54).contains(&weeks.len()), "{} Wochen", weeks.len());
    // Der Schlaf von vor dem Bereich reicht in ihn hinein.
    assert!(weeks[0]["asleep_h"].as_f64().unwrap() > 0.0, "{year}");
    assert!(
        weeks[weeks.len() - 5]["awake_h"].as_f64().unwrap() > 0.0,
        "{year}"
    );

    let all = fetch(&mut b, "all").await;
    assert_eq!(all["bucket"], "month", "{all}");
    assert_eq!(all["from"], all["first_day"], "{all}");
    let months = all["days"].as_array().unwrap();
    assert!((14..=15).contains(&months.len()), "{} Monate", months.len());

    // Zwei Tage von Hand, auch verkehrt herum.
    let from = year["days"][1]["date"].as_str().unwrap().to_string();
    let to = year["days"][2]["date"].as_str().unwrap().to_string();
    let picked = fetch(&mut b, &format!("{to}..{from}")).await;
    assert_eq!(picked["bucket"], "day");
    assert_eq!(picked["from"], from.as_str());
    assert_eq!(picked["to"], to.as_str());
    assert_eq!(picked["days"].as_array().unwrap().len(), 8);
    std::fs::remove_dir_all(&dir).unwrap();
}
