//! The overview page: its live stream and its buttons, against the simulated
//! PC with Strata and the PC service.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{AppState, clock::unix_now, state::PcState};

fn kind_list(v: &Value) -> Vec<String> {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

/// Seconds until the countdown ends, if it runs.
fn countdown(v: &Value) -> Option<i64> {
    let at = v["sleep"]["At"].as_u64()?;
    Some(at as i64 - v["now"].as_u64().unwrap() as i64)
}

// --- Live stream (#15) ---

#[tokio::test]
async fn stream_needs_a_login() {
    let s = setup(true, |_| {}).await;
    let anonymous = Browser::new(&s.url);
    assert_eq!(Live::request(&anonymous).await.status(), 401);
    let mut must_change = Browser::new(&s.url);
    must_change.call("login", &[("password", "admin")]).await;
    assert_eq!(Live::request(&must_change).await.status(), 403);
}

#[tokio::test]
async fn shows_an_awake_pc_and_a_ready_strata() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC awake", |v| v["pc"] == "Awake").await;
    assert_eq!(v["strata"], "Ready");
    assert_eq!(v["configured"], true);
    assert_eq!(v["mac_known"], true);
    assert_eq!(v["agent_configured"], true);
    assert_eq!(v["last_request"], Value::Null);
}

#[tokio::test]
async fn shows_a_sleeping_pc() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["strata"], "Offline");
    assert_eq!(v["sleep"], "Asleep");
}

#[tokio::test]
async fn shows_gaming_mode_noticed_at_the_pc() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let v = live.until("Gaming", |v| v["pc"] == "Gaming").await;
    assert_eq!(v["strata"], "Offline");
    assert_eq!(v["sleep"], "Gaming");
}

#[tokio::test]
async fn shows_the_last_request() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    post_json(&s, "/v1/messages", json!({})).await;
    let v = live
        .until("last request", |v| v["last_request"].is_string())
        .await;
    assert_eq!(v["last_request"].as_str().unwrap().len(), 5, "{v}");
}

#[tokio::test]
async fn sends_only_changes() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    // Nothing changes: no second overview, not even when the clock moves on.
    let next = tokio::time::timeout(Duration::from_millis(1500), live.next()).await;
    assert!(next.is_err(), "{:?}", next.ok());
}

#[tokio::test]
async fn asks_only_while_a_page_watches() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.state.overview.rounds(), 0);
    assert_eq!(s.pc.metrics_requests.load(Ordering::SeqCst), 0);

    let mut live = Live::open(&b).await;
    live.until("PC awake", |v| v["pc"] == "Awake").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        s.state.overview.rounds() >= 3,
        "{}",
        s.state.overview.rounds()
    );
    assert!(s.pc.metrics_requests.load(Ordering::SeqCst) >= 3);

    drop(live);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rounds = s.state.overview.rounds();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.state.overview.rounds(), rounds);
}

#[tokio::test]
async fn the_stream_carries_the_last_7_days_summed_once_a_minute() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC awake", |v| v["pc"] == "Awake").await;
    let week = &v["week"];
    assert_eq!(week["bucket"], "day", "{week}");
    assert_eq!(week["days"].as_array().unwrap().len(), 7, "{week}");
    assert!(
        week["totals"]["unknown_h"].as_f64().unwrap() > 0.0,
        "{week}"
    );
    // Many ticks of the stream, one sum.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(s.state.overview.rounds() >= 3);
    assert_eq!(s.state.overview.weeks_summed(), 1);
}

#[tokio::test]
async fn watching_neither_wakes_nor_keeps_awake() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);

    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(600)).await;
    let b = logged_in(&s.url).await;
    let _live = Live::open(&b).await;
    eventually("sleep request", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn shows_what_is_not_set_up() {
    let s = setup(true, |c| {
        c.strata_url = None;
        c.pc_mac = None;
        c.pc_agent_url = None;
    })
    .await;
    let b = logged_in(&s.url).await;
    let v = Live::open(&b).await.next().await;
    assert_eq!(v["configured"], false);
    assert_eq!(v["mac_known"], false);
    assert_eq!(v["agent_configured"], false);
    assert_eq!(v["sleep"], "NoAgent");
}

// --- Strata details (#16) ---

#[tokio::test]
async fn shows_running_and_waiting_requests_from_metrics() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("secret");
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
    // Requests that bypass the proxy do not hold off the sleep request.
    assert!(countdown(&v).is_some(), "{v}");
    assert_eq!(
        s.pc.metrics_auth.lock().unwrap().as_deref(),
        Some("Bearer secret")
    );

    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    live.until("ready", |v| v["strata"] == "Ready").await;
}

#[tokio::test]
async fn shows_loading_while_strata_starts() {
    let s = setup(true, |_| {}).await;
    s.pc.starting.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("loading", |v| v["strata"] == "Loading").await;
    s.pc.starting.store(false, Ordering::SeqCst);
    live.until("ready", |v| v["strata"] == "Ready").await;
}

#[tokio::test]
async fn shows_an_unloaded_model() {
    let s = setup(true, |_| {}).await;
    s.pc.unloaded.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("model unloaded", |v| v["strata"] == "Unloaded")
        .await;
}

#[tokio::test]
async fn counts_open_requests_without_metrics() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("ready", |v| v["strata"] == "Ready").await;
    let client = s.clone();
    let request = tokio::spawn(async move {
        post_json(
            &client,
            "/v1/messages",
            json!({"stream": true, "pause_ms": 1500}),
        )
        .await
        .text()
        .await
        .unwrap()
    });
    let v = live
        .until("busy", |v| v["strata"]["Busy"].is_object())
        .await;
    assert_eq!(
        v["strata"]["Busy"],
        json!({"processing": 1, "deferred": null, "tokens_per_sec": null})
    );
    request.await.unwrap();
    live.until("ready again", |v| v["strata"] == "Ready").await;
}

// --- Countdown (#17) ---

#[tokio::test]
async fn counts_down_to_sleep_and_follows_the_setting() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("Countdown", |v| countdown(v).is_some()).await;
    let left = countdown(&v).unwrap();
    assert!((3590..=3600).contains(&left), "{left}");

    let resp = save_settings(&mut b, &[("IDLE_TIMEOUT_SECS", "600")]).await;
    assert_eq!(resp.status(), 200);
    let v = live
        .until("shorter countdown", |v| {
            countdown(v).is_some_and(|l| l <= 600)
        })
        .await;
    assert!(countdown(&v).unwrap() >= 590);
}

#[tokio::test]
async fn shows_why_the_pc_refused_to_sleep() {
    let s = setup(true, |c| {
        c.idle_timeout = Duration::from_millis(300);
        c.sleep_retry = Duration::from_secs(60);
    })
    .await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("refusal", |v| v["sleep"]["Refused"].is_object())
        .await;
    assert_eq!(v["sleep"]["Refused"]["reason"], "User active");
    let retry = v["sleep"]["Refused"]["retry_at"].as_u64().unwrap();
    assert!(retry > unix_now() + 50, "{v}");
}

// --- Events (#18) ---

#[tokio::test]
async fn lists_the_latest_pc_events_newest_first() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    // An API request wakes the PC; the list fills without reloading.
    post_json(&s, "/v1/messages", json!({})).await;
    let v = live
        .until("awake", |v| kind_list(v).contains(&"awake".into()))
        .await;
    let kinds = kind_list(&v);
    assert_eq!(kinds[0], "awake", "{kinds:?}");
    assert!(kinds.contains(&"wake_sent".into()), "{kinds:?}");
    // The password change of the login is not about the PC.
    assert!(!kinds.contains(&"password_changed".into()), "{kinds:?}");
    assert!(v["events"][0]["time"].is_string());
}

// --- Wake and sleep (#19) ---

#[tokio::test]
async fn buttons_need_a_login() {
    let s = setup(false, |_| {}).await;
    let mut b = Browser::new(&s.url);
    for (name, fields) in [
        ("wake", vec![]),
        ("sleep-now", vec![]),
        ("gaming", vec![("enabled", "true")]),
        ("keep-awake", vec![("choice", "1h")]),
    ] {
        assert_eq!(b.call(name, &fields).await.status(), 401, "{name}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wake_button_wakes_and_idle_counts_from_then() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    assert_eq!(b.call("wake", &[]).await.status(), 200);
    let v = live
        .until("PC awake", |v| v["pc"] == "Awake" && countdown(v).is_some())
        .await;
    assert!(countdown(&v).unwrap() >= 3590);
    assert!(s.pc.up());
    let events = v["events"].as_array().unwrap();
    let requested = events
        .iter()
        .find(|e| e["kind"] == "wake_requested")
        .unwrap();
    assert_eq!(requested["detail"], "from the web UI");
}

#[tokio::test]
async fn wake_button_without_mac_says_why() {
    let s = setup(false, |c| c.pc_mac = None).await;
    let mut b = logged_in(&s.url).await;
    let resp = b.call("wake", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("MAC"));
}

#[tokio::test]
async fn sleep_button_puts_the_pc_to_sleep() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("sleep-now", &[]).await.status(), 200);
    assert!(!s.pc.up());
    let kinds: Vec<_> = own_kinds(&s.state)
        .into_iter()
        .map(|k| format!("{k:?}"))
        .collect();
    assert!(
        kinds.ends_with(&["SleepRequested".into(), "SleepAccepted".into()]),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn sleep_button_shows_a_refusal() {
    let s = setup(true, |_| {}).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let resp = b.call("sleep-now", &[]).await;
    // A refusal is an answer, with its reason apart for Force sleep.
    assert_eq!(resp.status(), 200);
    let answer: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(answer, json!({"Refused": {"reason": "User active"}}));
    assert!(s.pc.up());
}

// --- Gaming (#20) ---

#[tokio::test]
async fn gaming_on_wakes_a_sleeping_pc_first() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b.call("gaming", &[("enabled", "true")]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(s.pc.up());
    assert!(s.pc.gaming.load(Ordering::SeqCst));
    let mut live = Live::open(&b).await;
    let v = live.until("Gaming", |v| v["pc"] == "Gaming").await;
    let kinds = kind_list(&v);
    assert_eq!(
        kinds[..4],
        ["gaming_on", "awake", "wake_sent", "wake_requested"],
        "{kinds:?}"
    );
}

#[tokio::test]
async fn gaming_off_starts_the_idle_time_again() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Gaming", |v| v["pc"] == "Gaming").await;
    assert_eq!(
        b.call("gaming", &[("enabled", "false")]).await.status(),
        200
    );
    assert!(!s.pc.gaming.load(Ordering::SeqCst));
    let v = live
        .until("Countdown", |v| {
            v["pc"] == "Awake" && countdown(v).is_some()
        })
        .await;
    assert!(countdown(&v).unwrap() >= 3590);
}

#[tokio::test]
async fn gaming_on_reports_a_failed_wake() {
    let s = setup(false, |c| c.wake_timeout = Duration::from_millis(800)).await;
    s.pc.ignore_wol.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let resp = b.call("gaming", &[("enabled", "true")]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("did not wake in time"));
    assert!(!s.pc.gaming.load(Ordering::SeqCst));
    let kinds: Vec<_> = own_kinds(&s.state)
        .into_iter()
        .map(|k| format!("{k:?}"))
        .collect();
    assert!(!kinds.contains(&"GamingOn".into()), "{kinds:?}");
    // The history still shows that the page started the wake-up.
    let requested = s
        .state
        .events
        .recent()
        .into_iter()
        .find(|e| format!("{:?}", e.kind) == "WakeRequested")
        .expect("wake-up requested");
    assert_eq!(
        requested.detail.as_deref(),
        Some("for gaming mode, from the web UI")
    );
}

// --- Keep awake (#21) ---

#[tokio::test]
async fn keep_awake_holds_off_sleep_until_ended() {
    // Long enough for the login before the button, short enough to wait out.
    let s = setup(true, |c| c.idle_timeout = Duration::from_secs(2)).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        b.call("keep-awake", &[("choice", "1h")]).await.status(),
        200
    );
    let mut live = Live::open(&b).await;
    let v = live
        .until("kept awake", |v| v["sleep"]["KeptAwake"].is_object())
        .await;
    assert_eq!(v["sleep"]["KeptAwake"]["until"], v["keep_awake_until"]);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);

    assert_eq!(
        b.call("keep-awake", &[("choice", "off")]).await.status(),
        200
    );
    eventually("sleep request", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
    let kinds = kind_list(
        &live
            .until("event ended", |v| {
                kind_list(v).contains(&"keep_awake_ended".into())
            })
            .await,
    );
    assert!(kinds.contains(&"keep_awake_set".into()), "{kinds:?}");
}

#[tokio::test]
async fn keep_awake_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("strata-keep-awake-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.clone();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        b.call("keep-awake", &[("choice", "morning")])
            .await
            .status(),
        200
    );
    let cfg = (*s.state.cfg()).clone();
    let restarted = AppState::new(cfg.clone());
    let until = restarted
        .snapshot()
        .keep_awake_until
        .expect("still kept awake");
    let left = until
        .duration_since(std::time::SystemTime::now())
        .unwrap()
        .as_secs();
    assert!(left <= 24 * 3600, "{left}");

    // A value that ran out while the proxy was down is dropped.
    std::fs::write(dir.join("keep-awake.json"), r#"{"until": 1000}"#).unwrap();
    assert!(AppState::new(cfg).snapshot().keep_awake_until.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn keep_awake_rejects_unknown_choices() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    // Only the page's choices are understood.
    let resp = b.call("keep-awake", &[("choice", "99h")]).await;
    assert!(!resp.status().is_success(), "{}", resp.status());
    // Ending what was never set is no event.
    assert_eq!(
        b.call("keep-awake", &[("choice", "off")]).await.status(),
        200
    );
    assert!(s.state.events.latest_pc(10).is_empty());
}

// --- Review follow-ups ---

#[tokio::test]
async fn shows_a_running_request_as_the_reason_not_to_sleep() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Countdown", |v| countdown(v).is_some()).await;
    let client = s.clone();
    let request = tokio::spawn(async move {
        post_json(
            &client,
            "/v1/messages",
            json!({"stream": true, "pause_ms": 1500}),
        )
        .await
        .text()
        .await
        .unwrap()
    });
    live.until("request running", |v| v["sleep"] == "Busy")
        .await;
    request.await.unwrap();
    live.until("countdown again", |v| countdown(v).is_some())
        .await;
}

#[tokio::test]
async fn lists_events_from_before_a_restart() {
    let dir = data_dir("events");
    let data = dir.clone();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("sleep-now", &[]).await.status(), 200);
    let restarted = restarted(&s);
    let kinds: Vec<_> = restarted
        .events
        .latest_pc(10)
        .into_iter()
        .map(|e| format!("{:?}", e.kind))
        .collect();
    assert_eq!(kinds, ["SleepAccepted", "SleepRequested"], "{kinds:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn watching_after_a_start_does_not_restart_the_idle_time() {
    let s = setup(true, |_| {}).await;
    // The proxy just started and does not know the PC yet; a page opened
    // later finds it awake, which is no activity.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("Countdown", |v| countdown(v).is_some()).await;
    assert!(countdown(&v).unwrap() <= 3598, "{v}");
}

#[tokio::test]
async fn gaming_on_without_strata_address_says_why() {
    let s = setup(false, |c| c.strata_url = None).await;
    let mut b = logged_in(&s.url).await;
    let resp = b.call("gaming", &[("enabled", "true")]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("Strata address"));
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

// --- API keys (#27) ---

#[tokio::test]
async fn warns_while_no_api_key_exists() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("key exists", |v| v["api_key_exists"] == true)
        .await;
    let prefix = &s.key[..12];
    let resp = b
        .call("revoke-api-key", &[("name", "Test"), ("prefix", prefix)])
        .await;
    assert_eq!(resp.status(), 200);
    live.until("no key", |v| v["api_key_exists"] == false).await;
    b.call("create-api-key", &[("name", "Laptop")]).await;
    live.until("a key again", |v| v["api_key_exists"] == true)
        .await;
}

// --- Setup checklist (#65) ---

#[tokio::test]
async fn stream_tells_whether_data_survives_a_restart() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC awake", |v| v["pc"] == "Awake").await;
    assert_eq!(v["persistent"], false);

    let dir = std::env::temp_dir().join(format!("strata-overview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live.until("PC awake", |v| v["pc"] == "Awake").await;
    assert_eq!(v["persistent"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fresh_proxy_lists_what_is_left_to_set_up() {
    let s = setup(false, |c| c.strata_url = None).await;
    for k in s.state.api_keys.list() {
        s.state.api_keys.revoke(&k.name, &k.prefix).unwrap();
    }
    let b = logged_in(&s.url).await;
    let html = b.page("/admin").await;
    assert!(html.contains("Finish setup"), "{html}");
    assert!(html.contains("2 of 5 done"), "{html}");
    assert!(html.contains("Set address"), "{html}");
    assert!(html.contains("Create key"), "{html}");
    assert!(html.contains("How to"), "{html}");
    assert!(!html.contains("Set MAC"), "{html}");
    assert!(!html.contains(">Install<"), "{html}");
}

#[tokio::test]
async fn set_up_proxy_shows_no_checklist() {
    let dir = std::env::temp_dir().join(format!("strata-checklist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s = setup(false, |c| c.data_dir = Some(dir.clone())).await;
    s.state.api_keys.create("Laptop").unwrap();
    let b = logged_in(&s.url).await;
    let html = b.page("/admin").await;
    assert!(!html.contains("Finish setup"), "{html}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_missed_pc_service_answer_does_not_restart_the_idle_time() {
    let s = setup(true, |_| {}).await;
    eventually("PC awake", || s.state.snapshot().pc == PcState::Awake).await;
    let before = s.state.snapshot().last_activity_at;
    // Strata still answers; only the PC service misses its answers.
    s.pc.status_misses.store(3, Ordering::SeqCst);
    eventually("answers missed", || {
        s.pc.status_misses.load(Ordering::SeqCst) == 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.state.snapshot().pc, PcState::Awake);
    assert_eq!(s.state.snapshot().last_activity_at, before);
}
