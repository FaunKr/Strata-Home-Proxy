//! The settings page: every value with its origin, only after login.

mod common;

use common::*;
use serde_json::Value;
use strata_nas_proxy::{AppState, Config, Settings, api_router};

async fn settings(b: &mut Browser) -> Value {
    let resp = b.call("settings", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

fn entry<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == key)
        .unwrap_or_else(|| panic!("{key} fehlt"))
}

#[tokio::test]
async fn settings_need_a_login() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(b.call("settings", &[]).await.status(), 401);
    let html = b.page("/admin/system").await;
    assert!(html.contains("Sign in"), "{html}");
    assert!(!html.contains("Proxy address"), "{html}");

    // With the default password still in place: change it first.
    b.call("login", &[("password", "admin")]).await;
    assert_eq!(b.call("settings", &[]).await.status(), 403);
}

#[tokio::test]
async fn shows_every_value_with_its_origin() {
    let dir = std::env::temp_dir().join(format!("strata-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut base = Config::default();
    base.apply("STRATA_URL", "http://pc:8080").unwrap();
    base.apply("PC_AGENT_TOKEN", "streng-geheim").unwrap();
    base.data_dir = Some(dir.clone());
    let state = AppState::with_settings(Settings::new(base, &["STRATA_URL", "PC_AGENT_TOKEN"]));
    state.settings.set("IDLE_TIMEOUT_SECS", "600").unwrap();
    let url = format!("http://{}", serve(api_router(state)).await);
    let mut b = logged_in(&url).await;

    let view = settings(&mut b).await;
    assert_eq!(view["persistent"], true);
    assert_eq!(view["configured"], true);
    assert_eq!(view["entries"].as_array().unwrap().len(), 17);
    assert!(!view.to_string().contains("DATA_DIR"));
    assert_eq!(entry(&view, "STRATA_URL")["source"], "Env");
    assert_eq!(entry(&view, "STRATA_URL")["value"], "http://pc:8080");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["value"], "600");
    assert_eq!(entry(&view, "WAKE_TIMEOUT_SECS")["source"], "Default");
    assert_eq!(entry(&view, "LISTEN_ADDR")["restart"], true);
    assert_eq!(entry(&view, "PC_AGENT_TOKEN")["value"], "set");
    assert!(!view.to_string().contains("streng-geheim"));

    let html = b.page("/admin/strata").await;
    assert!(html.contains("Strata address"), "{html}");
    assert!(html.contains("Environment"), "{html}");
    let html = b.page("/admin/system").await;
    assert!(html.contains("Takes effect after a restart"), "{html}");
    let html = b.page("/admin/pc").await;
    assert!(html.contains("Saved here"), "{html}");
    assert!(!html.contains("streng-geheim"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn system_explains_a_missing_data_dir() {
    let s = setup(true, |c| c.strata_url = None).await;
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    assert_eq!(view["persistent"], false);
    assert_eq!(view["configured"], false);
    let html = b.page("/admin/system").await;
    assert!(html.contains("DATA_DIR is not set"), "{html}");
}

#[tokio::test]
async fn each_area_shows_its_own_settings() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let areas = [
        (
            "/admin/pc",
            &[
                "PC_MAC",
                "WOL_ADDR",
                "PC_AGENT_URL",
                "PC_AGENT_TOKEN",
                "IDLE_TIMEOUT_SECS",
                "WAKE_TIMEOUT_SECS",
                "WOL_RESEND_SECS",
                "HEALTH_POLL_MS",
                "IDLE_CHECK_SECS",
                "SLEEP_RETRY_SECS",
                "SLEEP_SETTLE_SECS",
            ][..],
        ),
        ("/admin/strata", &["STRATA_URL", "STRATA_API_KEY"][..]),
        (
            "/admin/system",
            &[
                "LISTEN_ADDR",
                "MAX_BODY_BYTES",
                "PING_INTERVAL_SECS",
                "PROBE_TIMEOUT_MS",
            ][..],
        ),
    ];
    let all: Vec<&str> = areas
        .iter()
        .flat_map(|(_, keys)| keys.iter().copied())
        .collect();
    assert_eq!(all.len(), 17);
    for (path, keys) in areas {
        let html = b.page(path).await;
        for key in &all {
            let field = format!("id=\"setting-{key}\"");
            assert_eq!(html.contains(&field), keys.contains(key), "{path}: {key}");
        }
    }
    assert!(!b.page("/admin/access").await.contains("id=\"setting-"));
}

#[tokio::test]
async fn reported_mac_is_shown_above_the_saved_one() {
    let s = setup(true, |_| {}).await;
    s.state.settings.set("PC_MAC", "01:02:03:04:05:06").unwrap();
    s.state.settings.set_reported_mac([0xaa; 6]);
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    let mac = entry(&view, "PC_MAC");
    assert_eq!(mac["source"], "Reported");
    assert_eq!(mac["value"], "AA:AA:AA:AA:AA:AA");
    assert_eq!(mac["saved"], "01:02:03:04:05:06");
    let html = b.page("/admin/pc").await;
    assert!(html.contains("Reported by PC"), "{html}");
    assert!(html.contains("only a fallback"), "{html}");
    assert!(html.contains("value=\"01:02:03:04:05:06\""), "{html}");
}

async fn save(b: &mut Browser, key: &str, value: &str) -> reqwest::Response {
    save_settings(b, &[(key, value)]).await
}

#[tokio::test]
async fn a_section_is_saved_in_one_call() {
    use strata_nas_proxy::events::EventKind;
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(
        &mut b,
        &[("IDLE_TIMEOUT_SECS", "600"), ("WAKE_TIMEOUT_SECS", "99")],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let cfg = s.state.cfg();
    assert_eq!(cfg.idle_timeout.as_secs(), 600);
    assert_eq!(cfg.wake_timeout.as_secs(), 99);
    let view = settings(&mut b).await;
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
    assert_eq!(entry(&view, "WAKE_TIMEOUT_SECS")["source"], "Saved");
    let saved: Vec<_> = s
        .state
        .events
        .recent()
        .into_iter()
        .filter(|e| e.kind == EventKind::SettingSaved)
        .filter_map(|e| e.detail)
        .collect();
    assert_eq!(saved.len(), 2, "{saved:?}");
}

#[tokio::test]
async fn one_invalid_value_saves_none_and_names_its_key() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(&mut b, &[("IDLE_TIMEOUT_SECS", "600"), ("PC_MAC", "12:34")]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("PC_MAC: "));
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
    let view = settings(&mut b).await;
    assert_ne!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
}

#[tokio::test]
async fn an_empty_secret_stays_as_it_is() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save(&mut b, "PC_AGENT_TOKEN", "streng-geheim")
            .await
            .status(),
        200
    );
    let resp = save_settings(
        &mut b,
        &[("PC_AGENT_TOKEN", ""), ("IDLE_TIMEOUT_SECS", "5")],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let cfg = s.state.cfg();
    assert_eq!(cfg.pc_agent_token.as_deref(), Some("streng-geheim"));
    assert_eq!(cfg.idle_timeout.as_secs(), 5);

    // Only a reset removes it.
    b.call("reset-setting", &[("key", "PC_AGENT_TOKEN")]).await;
    assert_ne!(
        s.state.cfg().pc_agent_token.as_deref(),
        Some("streng-geheim")
    );
}

#[tokio::test]
async fn saving_and_resetting_need_a_login() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(save(&mut b, "IDLE_TIMEOUT_SECS", "1").await.status(), 401);
    assert_eq!(
        b.call("reset-setting", &[("key", "IDLE_TIMEOUT_SECS")])
            .await
            .status(),
        401
    );
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
}

#[tokio::test]
async fn saved_strata_url_applies_at_once_and_reset_restores_it() {
    use std::sync::{Arc, atomic::Ordering};
    let s = setup(true, |_| {}).await;
    let other = Arc::new(FakePc::default());
    other.awake.store(true, Ordering::SeqCst);
    let other_strata = serve_strata(other.clone()).await;
    let mut b = logged_in(&s.url).await;

    let resp = save(&mut b, "STRATA_URL", &format!("http://{other_strata}")).await;
    assert_eq!(resp.status(), 200);
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(other.api_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        entry(&settings(&mut b).await, "STRATA_URL")["source"],
        "Saved"
    );

    let resp = b.call("reset-setting", &[("key", "STRATA_URL")]).await;
    assert_eq!(resp.status(), 200);
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    assert_ne!(
        entry(&settings(&mut b).await, "STRATA_URL")["source"],
        "Saved"
    );
}

#[tokio::test]
async fn saved_idle_timeout_brings_sleep_forward() {
    use std::{sync::atomic::Ordering, time::Duration};
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(save(&mut b, "IDLE_TIMEOUT_SECS", "1").await.status(), 200);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_values_are_rejected_with_a_message() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    for (key, value) in [
        ("IDLE_TIMEOUT_SECS", "bald"),
        ("PC_MAC", "12:34"),
        ("STRATA_URL", "pc:8080"),
        ("WOL_ADDR", "192.168.1.255"),
        ("IDLE_TIMEOUT_SECS", ""),
        ("DATA_DIR", "/tmp"),
    ] {
        let resp = save(&mut b, key, value).await;
        assert_eq!(resp.status(), 400, "{key}={value}");
        assert!(!resp.text().await.unwrap().is_empty());
    }
    let resp = save(&mut b, "IDLE_TIMEOUT_SECS", "bald").await;
    assert!(resp.text().await.unwrap().contains("whole number"));
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
    assert!(
        s.state
            .settings
            .entries()
            .iter()
            .all(|e| e.source != strata_nas_proxy::settings::Source::Saved)
    );
}

#[tokio::test]
async fn changes_are_logged_without_secrets() {
    use strata_nas_proxy::events::EventKind;
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save(&mut b, "PC_AGENT_TOKEN", "streng-geheim")
            .await
            .status(),
        200
    );
    b.call("reset-setting", &[("key", "PC_AGENT_TOKEN")]).await;
    let events = s.state.events.recent();
    let changes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::SettingSaved | EventKind::SettingReset))
        .collect();
    assert_eq!(changes.len(), 2);
    assert!(
        changes
            .iter()
            .all(|e| e.detail.as_deref() == Some("PC_AGENT_TOKEN"))
    );
    let all = serde_json::to_string(&events).unwrap();
    assert!(!all.contains("streng-geheim"));
}

#[tokio::test]
async fn saved_values_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("strata-page-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = Config {
        data_dir: Some(dir.clone()),
        ..Config::default()
    };
    let first = AppState::new(base.clone());
    let mut b = logged_in(&format!("http://{}", serve(api_router(first)).await)).await;
    assert_eq!(save(&mut b, "WAKE_TIMEOUT_SECS", "99").await.status(), 200);
    let second = AppState::new(base);
    assert_eq!(second.cfg().wake_timeout.as_secs(), 99);
    std::fs::remove_dir_all(&dir).unwrap();
}
