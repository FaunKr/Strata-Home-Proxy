//! Settings changed at runtime take effect without a restart.

mod common;

use std::{sync::Arc, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{AppState, Config, Settings, proxy::STATUS_HEADER, settings::Source};

#[tokio::test]
async fn changed_strata_url_applies_at_once() {
    let s = setup(true, |_| {}).await;
    let other = Arc::new(FakePc::default());
    other.awake.store(true, Ordering::SeqCst);
    let other_strata = serve_strata(other.clone()).await;
    s.state
        .settings
        .set("STRATA_URL", &format!("http://{other_strata}"))
        .unwrap();
    let resp = post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(other.api_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);

    // Back to the environment value.
    s.state.settings.reset("STRATA_URL").unwrap();
    post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shorter_idle_timeout_applies_at_once() {
    let s = setup(true, |_| {}).await;
    post_json(&s.url, "/v1/messages", json!({})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    s.state.settings.set("IDLE_TIMEOUT_SECS", "1").unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_mac_goes_into_the_next_magic_packet() {
    let s = setup(false, |_| {}).await;
    let other = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    *s.pc.wake_mac.lock().unwrap() = Some(other);
    s.state.settings.set("PC_MAC", "AA:BB:CC:DD:EE:FF").unwrap();
    let resp = post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(*s.pc.last_mac.lock().unwrap(), Some(other));
}

#[tokio::test]
async fn proxy_without_strata_url_says_it_is_not_set_up() {
    let s = setup(true, |c| c.strata_url = None).await;
    let resp = post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()[STATUS_HEADER], "not_configured");
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nicht eingerichtet")
    );
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn waking_without_mac_fails_clearly() {
    let s = setup(false, |c| c.pc_mac = None).await;
    let resp = post_json(&s.url, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()[STATUS_HEADER], "no_mac");
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"].as_str().unwrap().contains("MAC"));
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn saved_values_are_seen_by_a_second_proxy() {
    let dir = std::env::temp_dir().join(format!("strata-e2e-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = Config {
        data_dir: Some(dir.clone()),
        ..Config::default()
    };
    let first = AppState::new(base.clone());
    first.settings.set("WAKE_TIMEOUT_SECS", "77").unwrap();
    let second = AppState::with_settings(Settings::new(base, &["STRATA_URL"]));
    assert_eq!(second.cfg().wake_timeout, Duration::from_secs(77));
    let entries = second.settings.entries();
    let wake = entries
        .iter()
        .find(|e| e.field.key == "WAKE_TIMEOUT_SECS")
        .unwrap();
    assert_eq!(wake.source, Source::Saved);
    assert_eq!(wake.value.as_deref(), Some("77"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn changed_wol_addr_goes_into_the_next_magic_packet() {
    let s = setup(false, |c| c.wol_addr = "127.0.0.1:9".parse().unwrap()).await;
    let wol = wol_listener(s.pc.clone()).await;
    s.state.settings.set("WOL_ADDR", &wol.to_string()).unwrap();
    let resp = post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
}
