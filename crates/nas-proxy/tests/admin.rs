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
        "/admin/strata-log",
        "/admin/api/strata-output-stream",
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

/// Server functions meant to wake the PC; their own tests cover them.
const WAKES: &[&str] = &["wake", "strata-job"];

#[tokio::test]
async fn no_server_function_wakes_the_pc() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut names: Vec<&str> = strata_admin_ui::server::server_fn_paths()
        .into_iter()
        .filter_map(|path| path.strip_prefix("/admin/api/"))
        .filter(|name| !WAKES.contains(name))
        .collect();
    // Logging out last keeps the session for the others.
    names.sort_by_key(|name| (*name == "logout", *name));
    assert!(names.len() > 10, "{names:?}");
    for name in names {
        // Without fields, functions with arguments stop at decoding; the rest run.
        let status = b.call(name, &[]).await.status();
        assert_ne!(status, 502, "{name}");
        assert_ne!(status, 404, "{name}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn managing_api_keys_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("api-keys", &[]).await.status(), 200);
    let created: serde_json::Value = b
        .call("create-api-key", &[("name", "Laptop")])
        .await
        .json()
        .await
        .unwrap();
    let prefix = created["info"]["prefix"].as_str().unwrap();
    let resp = b
        .call("revoke-api-key", &[("name", "Laptop"), ("prefix", prefix)])
        .await;
    assert_eq!(resp.status(), 200);
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

/// Building a router lists the pages by rendering the app once, and Leptos
/// holds back every resource in the process while it does (#69). A page
/// served meanwhile must still finish: tests build their own routers in
/// parallel.
#[tokio::test]
async fn a_page_finishes_while_another_router_is_built() {
    use std::sync::{Arc, atomic::AtomicBool};
    let s = setup(true, |_| {}).await;
    let stop = Arc::new(AtomicBool::new(false));
    let builder = {
        let (stop, state) = (stop.clone(), s.state.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _rt = rt.enter();
            while !stop.load(Ordering::SeqCst) {
                drop(strata_nas_proxy::api_router(state.clone()));
            }
        })
    };
    let b = Browser::new(&s.url);
    for _ in 0..20 {
        let html = tokio::time::timeout(Duration::from_secs(5), b.page("/admin/"))
            .await
            .expect("Seite hängt");
        assert!(html.contains("Anmelden"), "{html}");
    }
    stop.store(true, Ordering::SeqCst);
    builder.join().unwrap();
}
