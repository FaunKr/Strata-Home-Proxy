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
    let html = b.page("/admin/settings").await;
    assert!(html.contains("Anmelden"), "{html}");
    assert!(!html.contains("Strata-Adresse"), "{html}");

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
    assert_eq!(view["entries"].as_array().unwrap().len(), 16);
    assert!(!view.to_string().contains("DATA_DIR"));
    assert_eq!(entry(&view, "STRATA_URL")["source"], "Env");
    assert_eq!(entry(&view, "STRATA_URL")["value"], "http://pc:8080");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["value"], "600");
    assert_eq!(entry(&view, "WAKE_TIMEOUT_SECS")["source"], "Default");
    assert_eq!(entry(&view, "LISTEN_ADDR")["restart"], true);
    assert_eq!(entry(&view, "PC_AGENT_TOKEN")["value"], "gesetzt");
    assert!(!view.to_string().contains("streng-geheim"));

    let html = b.page("/admin/settings").await;
    assert!(html.contains("Strata-Adresse"), "{html}");
    assert!(html.contains("aus Umgebungsvariable"), "{html}");
    assert!(html.contains("Gilt nach Neustart"), "{html}");
    assert!(!html.contains("streng-geheim"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn warns_without_data_dir_and_without_strata_url() {
    let s = setup(true, |c| c.strata_url = None).await;
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    assert_eq!(view["persistent"], false);
    assert_eq!(view["configured"], false);
    let html = b.page("/admin/settings").await;
    assert!(html.contains("DATA_DIR ist nicht gesetzt"), "{html}");
    assert!(html.contains("noch nicht eingerichtet"), "{html}");
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
    let html = b.page("/admin/settings").await;
    assert!(html.contains("vom PC gemeldet"), "{html}");
    assert!(html.contains("Ersatzwert"), "{html}");
}
