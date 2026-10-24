//! Service tests with a fake PC and a fake Strata on localhost.

use crate::common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};

#[tokio::test]
async fn token_is_required() {
    let s = setup("token").await;
    let resp = client()
        .get(format!("{}/status", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client()
        .get(format!("{}/status", s.url))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let status: PcStatus = client()
        .get(format!("{}/status", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!status.gaming_mode);
    assert!(status.strata_running);
    assert!(status.strata_answers);
}

#[tokio::test]
async fn sleep_is_accepted_when_nothing_holds_it() {
    let s = setup("accept").await;
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 200);
    assert_eq!(body["accepted"], true);
    // The answer goes out first, standby follows a moment later.
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 0);
    eventually("standby", || s.fake.suspends.load(Ordering::SeqCst) == 1).await;
}

#[tokio::test]
async fn fresh_strata_after_standby_restarts_it_around_sleep() {
    let mut s = setup("fresh").await;
    let mut cfg = s.agent.cfg.clone();
    cfg.fresh_strata_after_standby = true;
    s.agent = Agent::new(cfg, Box::new(FakePlatform(s.fake.clone())));
    let addr = serve(router(s.agent.clone())).await;
    s.url = format!("http://{addr}");

    let (code, _) = sleep_answer(&s).await;
    assert_eq!(code, 200);
    eventually("Strata started again after standby", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);

    // A resume the watchdog notices right after does not start a second copy.
    s.agent.resumed();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sleep_is_refused_while_someone_works() {
    let s = setup("user").await;
    *s.fake.idle.lock().unwrap() = Duration::from_secs(30);
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("using the PC"));
}

#[tokio::test]
async fn sleep_is_refused_while_strata_works() {
    let s = setup("busy").await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("Strata"));
}

#[tokio::test]
async fn gaming_mode_stops_strata_blocks_sleep_and_survives_restart() {
    let s = setup("gaming").await;
    let status: PcStatus = client()
        .post(format!("{}/gaming", s.url))
        .bearer_auth("secret")
        .json(&json!({"enabled": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status.gaming_mode);
    assert!(!status.strata_running);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);

    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert!(body["reason"].as_str().unwrap().contains("Gaming"));

    // The watchdog leaves Strata off while gaming.
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);

    // A new service process (e.g. after a reboot) still knows gaming is on.
    let again = s.restarted();
    assert!(again.gaming());

    s.agent.set_gaming(false);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert!(s.fake.strata_up.load(Ordering::SeqCst));
    let again = s.restarted();
    assert!(!again.gaming());
}

#[tokio::test]
async fn watchdog_restarts_unresponsive_strata_after_load_time() {
    let s = setup("watchdog").await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    // First pass only notes the failure; the next one (restart_after_secs = 0) restarts.
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);

    // While the model loads (load_time_secs = 1) there is no second restart.
    s.fake.strata_up.store(false, Ordering::SeqCst);
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn healthy_strata_is_left_alone() {
    let s = setup("healthy").await;
    for _ in 0..3 {
        s.agent.watchdog_tick().await;
    }
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

async fn status_json(s: &Setup) -> Value {
    client()
        .get(format!("{}/status", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn status_reports_the_mac_of_the_network_card() {
    let s = setup("mac").await;
    *s.fake.mac.lock().unwrap() = Some([0xaa, 0xbb, 0xcc, 1, 2, 3]);
    assert_eq!(status_json(&s).await["mac"], "AA:BB:CC:01:02:03");
    // Looked up once, then reused.
    assert_eq!(status_json(&s).await["mac"], "AA:BB:CC:01:02:03");
    assert_eq!(s.fake.mac_lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn status_leaves_out_an_unknown_mac() {
    let s = setup("no-mac").await;
    let status = status_json(&s).await;
    assert!(status.get("mac").is_none(), "{status}");
    assert_eq!(status["gaming_mode"], false);
}

#[tokio::test]
async fn configured_mac_replaces_the_found_one() {
    let s = setup_with("mac-config", |c| c.mac = Some("11-22-33-44-55-66".into())).await;
    *s.fake.mac.lock().unwrap() = Some([9; 6]);
    assert_eq!(status_json(&s).await["mac"], "11:22:33:44:55:66");
    assert_eq!(s.fake.mac_lookups.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn managed_values_come_before_the_configuration() {
    let s = setup_with("managed", |cfg| {
        let managed = json!({
            "strata_start": "C:\\Strata\\run-qwen.bat",
            "strata_processes": ["C:\\Strata\\"],
        });
        let dir = cfg.state_dir.clone().unwrap();
        std::fs::write(dir.join("strata-managed.json"), managed.to_string()).unwrap();
        cfg.strata_args = vec!["--from-the-toml".into()];
    })
    .await;
    s.agent.set_gaming(true);
    assert_eq!(
        s.fake.stopped.lock().unwrap().clone().unwrap(),
        ["C:\\Strata\\"]
    );
    s.agent.set_gaming(false);
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, PathBuf::from("C:\\Strata\\run-qwen.bat"));
    // Arguments from the TOML belong to its start script, not the managed one.
    assert!(args.is_empty(), "{args:?}");
}

#[tokio::test]
async fn without_managed_values_the_configuration_applies() {
    let s = setup("unmanaged").await;
    s.agent.set_gaming(true);
    s.agent.set_gaming(false);
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, PathBuf::from("START-HERE.bat"));
    assert_eq!(
        s.fake.stopped.lock().unwrap().clone().unwrap(),
        ["llama-server.exe"]
    );
}

#[tokio::test]
async fn without_a_start_script_the_service_runs_but_leaves_strata_alone() {
    // The one-line setup leaves start script and programs empty; the Strata
    // page sets them later.
    let s = setup_with("no-start", |cfg| {
        cfg.strata_start = None;
        cfg.strata_processes = vec![];
    })
    .await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    for _ in 0..3 {
        s.agent.watchdog_tick().await;
    }
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);

    let status = status_json(&s).await;
    assert_eq!(status["strata_running"], false);
    let (code, _) = sleep_answer(&s).await;
    assert_eq!(code, 200);

    // Gaming on and off works and starts or ends nothing.
    s.agent.set_gaming(true);
    assert!(s.agent.gaming());
    s.agent.set_gaming(false);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn managed_start_script_applies_without_one_in_the_configuration() {
    let s = setup_with("managed-only", |cfg| {
        cfg.strata_start = None;
        cfg.strata_processes = vec![];
        let managed = json!({
            "strata_start": "C:\\Strata\\run-qwen.bat",
            "strata_processes": ["C:\\Strata\\"],
        });
        let dir = cfg.state_dir.clone().unwrap();
        std::fs::write(dir.join("strata-managed.json"), managed.to_string()).unwrap();
    })
    .await;
    s.agent.set_gaming(true);
    s.agent.set_gaming(false);
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, PathBuf::from("C:\\Strata\\run-qwen.bat"));
}

/// With Strata gone, connecting can hang (Windows retries a refused
/// connection to localhost for 2 s); `/status` still answers well within the
/// proxy's 2 s, or the PC looks asleep.
#[tokio::test]
async fn status_answers_quickly_while_strata_does_not_take_connections() {
    let s = setup_with("status-quick", |c| {
        // Not routable: the connection neither opens nor fails.
        c.strata_url = "http://10.255.255.1:8080".into();
    })
    .await;
    let started = std::time::Instant::now();
    let status = status_json(&s).await;
    assert_eq!(status["strata_running"], false);
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

// --- Forced sleep and forced Strata restart ---

#[tokio::test]
async fn forced_sleep_overrides_activity_a_busy_strata_and_gaming_mode() {
    let s = setup("force-sleep").await;
    *s.fake.idle.lock().unwrap() = Duration::from_secs(30);
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, body) = post(&s, "/sleep/force", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        body,
        json!({"user_active": true, "strata_busy": true, "gaming_mode": false})
    );
    // The answer goes out first, standby follows as after `/sleep`.
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 0);
    eventually("standby", || s.fake.suspends.load(Ordering::SeqCst) == 1).await;
}

#[tokio::test]
async fn forced_sleep_leaves_gaming_mode_on() {
    let s = setup("force-sleep-gaming").await;
    s.agent.set_gaming(true);
    let (code, body) = post(&s, "/sleep/force", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["gaming_mode"], true);
    assert_eq!(body["user_active"], false);
    eventually("standby", || s.fake.suspends.load(Ordering::SeqCst) == 1).await;
    // Back from standby: still gaming, and Strata stays off. A start would
    // follow the standby at once; the wait gives it time to show.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(s.agent.gaming());
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert!(s.restarted().gaming());
}

#[tokio::test]
async fn fresh_strata_after_standby_applies_to_a_forced_sleep() {
    let s = setup_with("force-sleep-fresh", |c| c.fresh_strata_after_standby = true).await;
    *s.fake.idle.lock().unwrap() = Duration::from_secs(30);
    let (code, _) = post(&s, "/sleep/force", json!({})).await;
    assert_eq!(code, 200);
    eventually("Strata started again after standby", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forced_restart_kills_and_starts_strata_also_when_it_hangs() {
    let s = setup("force-restart").await;
    // Strata runs but does not answer.
    s.fake.strata_up.store(false, Ordering::SeqCst);
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, json!({"started": true}));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    // The configured programs end, whoever started them.
    assert_eq!(
        s.fake.stopped.lock().unwrap().clone().unwrap(),
        ["llama-server.exe"]
    );
}

#[tokio::test]
async fn the_status_shows_a_freshly_started_strata_as_running() {
    let s = setup_with("force-restart-loading", |c| c.load_time_secs = 30).await;
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let (code, _) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200);
    // It does not answer yet, but it runs: the page shows it loading.
    let status = status_json(&s).await;
    assert_eq!(status["strata_running"], true);
    assert_eq!(status["strata_answers"], false);
}

#[tokio::test]
async fn forced_restart_without_a_start_script_leaves_strata_stopped() {
    let s = setup_with("force-restart-no-start", |cfg| cfg.strata_start = None).await;
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, json!({"started": false}));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn forced_restart_with_auto_start_off_leaves_strata_stopped() {
    let s = setup_with("force-restart-no-auto", |cfg| cfg.auto_start = false).await;
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["started"], false);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn forced_restart_is_refused_in_gaming_mode() {
    let s = setup("force-restart-gaming").await;
    s.agent.set_gaming(true);
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 409);
    assert_eq!(body["reason"], "Gaming mode is on");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_watchdog_leaves_a_forced_restart_alone_while_it_loads() {
    let s = setup_with("force-restart-watchdog", |c| c.load_time_secs = 30).await;
    // The new Strata does not answer; the watchdog (restart_after_secs = 0)
    // would restart it on its next pass, were it not loading.
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let (code, _) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200);
    for _ in 0..3 {
        s.agent.watchdog_tick().await;
    }
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_second_sleep_while_standby_is_pending_brings_no_second_standby() {
    let s = setup("force-sleep-twice").await;
    // A double click, a second tab, and the idle check, all within the delay.
    for path in ["/sleep/force", "/sleep/force", "/sleep"] {
        let (code, body) = post(&s, path, json!({})).await;
        assert_eq!(code, 200, "{path}: {body}");
    }
    eventually("standby", || s.fake.suspends.load(Ordering::SeqCst) == 1).await;
    // A second sleep would have started its own 2 s delay with the first
    // request, so its standby would follow the first one at once.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_stopped_strata_does_not_count_as_running_within_its_load_time() {
    let s = setup_with("force-restart-stopped", |c| c.load_time_secs = 30).await;
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let (code, _) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200);
    assert_eq!(status_json(&s).await["strata_running"], true);
    s.agent.strata().stop();
    assert_eq!(status_json(&s).await["strata_running"], false);
}

#[tokio::test]
async fn forced_restart_reports_a_failed_start_as_not_started() {
    let s = setup("force-restart-start-fails").await;
    s.fake.start_fails.store(true, Ordering::SeqCst);
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, json!({"started": false}));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
}
