//! The web UI under `/admin/`: answered by the proxy itself, never forwarded,
//! never wakes the PC and never counts as activity.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;

async fn get(url: &str) -> reqwest::Response {
    reqwest::get(url).await.unwrap()
}

#[tokio::test]
async fn admin_page_is_html() {
    let s = setup(true, |_| {}).await;
    let resp = get(&format!("{}/admin/", s.url)).await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let html = resp.text().await.unwrap();
    assert!(html.contains("Strata-Proxy"), "{html}");
    assert!(html.contains("/admin/pkg/admin-ui.js"), "{html}");
}

#[tokio::test]
async fn admin_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    for path in [
        "/admin",
        "/admin/",
        "/admin/gibt-es-nicht",
        "/admin/api-keys",
        "/admin/pkg/admin-ui.js",
    ] {
        let resp = get(&format!("{}{path}", s.url)).await;
        assert_ne!(resp.status(), 502, "{path}");
    }
    let resp = get(&format!("{}/admin/gibt-es-nicht", s.url)).await;
    assert_eq!(resp.status(), 404);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn managing_api_keys_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    for (call, args) in [
        ("api-keys", &[][..]),
        ("create-api-key", &[("name", "Laptop")][..]),
        ("revoke-api-key", &[("name", "Laptop")][..]),
    ] {
        assert_eq!(b.call(call, args).await.status(), 200, "{call}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn admin_does_not_postpone_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(600)).await;
    // Keep the UI busy for longer than the idle timeout.
    for _ in 0..8 {
        get(&format!("{}/admin/", s.url)).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.state.in_flight(), 0);
}
