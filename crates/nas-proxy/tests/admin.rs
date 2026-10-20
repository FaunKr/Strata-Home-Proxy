//! The web UI under `/admin/`: answered by the proxy itself, never forwarded,
//! never wakes the PC and never counts as activity.

use crate::common;

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
    assert!(html.contains("Strata Proxy"), "{html}");
    assert!(html.contains("/admin/pkg/admin-ui.js"), "{html}");
}

#[tokio::test]
async fn admin_does_not_wake_the_pc() {
    let s = setup(false, |_| {}).await;
    for path in [
        "/admin",
        "/admin/",
        "/admin/does-not-exist",
        "/admin/pc",
        "/admin/pc/history",
        "/admin/strata",
        "/admin/strata/output",
        "/admin/access",
        "/admin/system",
        "/admin/api/strata-output-stream",
        "/admin/pkg/admin-ui.js",
    ] {
        let resp = get(&format!("{}{path}", s.url)).await;
        assert_ne!(resp.status(), 502, "{path}");
    }
    // The paths before the areas are gone.
    for path in [
        "/admin/does-not-exist",
        "/admin/settings",
        "/admin/api-keys",
        "/admin/password",
        "/admin/pc-dienst",
        "/admin/strata-log",
    ] {
        let resp = get(&format!("{}{path}", s.url)).await;
        assert_eq!(resp.status(), 404, "{path}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 0);
}

/// Server functions meant to wake the PC; their own tests cover them.
const WAKES: &[&str] = &[
    "wake",
    "strata-job",
    "strata-update-summary",
    "pc-service-update",
];

#[tokio::test]
async fn no_server_function_wakes_the_pc() {
    let s = setup(false, |_| {}).await;
    // An update waiting in the image, so the update button would get as far
    // as waking the PC if it were not in WAKES.
    s.state
        .pc_setup
        .set_program(agent_program("no-wake", "0.1.0+waiting"));
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

#[tokio::test]
async fn every_area_shows_the_frame_with_the_state_badges() {
    let s = setup(true, |_| {}).await;
    let anonymous = Browser::new(&s.url);
    let html = anonymous.page("/admin/pc").await;
    assert!(html.contains("Sign in"), "{html}");
    assert!(!html.contains("class=\"badges\""), "{html}");

    let b = logged_in(&s.url).await;
    for (path, heading) in [
        ("/admin/", "Overview"),
        ("/admin/pc", "PC"),
        ("/admin/pc/history", "PC"),
        ("/admin/strata", "Strata"),
        ("/admin/strata/output", "Strata"),
        ("/admin/strata/version", "Strata"),
        ("/admin/strata/connection", "Strata"),
        ("/admin/access", "Access"),
        ("/admin/system", "System"),
    ] {
        let html = b.page(path).await;
        assert!(html.contains("class=\"badges\""), "{path}: {html}");
        assert!(html.contains("PC · "), "{path}: {html}");
        assert!(html.contains("Strata · "), "{path}: {html}");
        assert!(html.contains("aria-label=\"Main\""), "{path}: {html}");
        assert!(html.contains(&format!("<h1>{heading}")), "{path}: {html}");
    }
}

/// Building a router lists the pages by rendering the app once, and Leptos
/// holds back every resource in the process while it does (#69). A page
/// served meanwhile must still finish: tests build their own routers in
/// parallel.
#[tokio::test]
async fn a_page_finishes_while_another_router_is_built() {
    use std::sync::{Arc, atomic::AtomicBool};
    let s = setup(true, |_| {}).await;
    // Stops the builder also when an assert below fails.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let stop = Stop(Arc::new(AtomicBool::new(false)));
    let builder = {
        let (stop, state) = (stop.0.clone(), s.state.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _rt = rt.enter();
            while !stop.load(Ordering::SeqCst) {
                drop(strata_nas_proxy::api_router(state.clone()));
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let b = Browser::new(&s.url);
    for _ in 0..20 {
        let html = tokio::time::timeout(Duration::from_secs(5), b.page("/admin/"))
            .await
            .expect("page hangs");
        assert!(html.contains("Sign in"), "{html}");
    }
    drop(stop);
    builder.join().unwrap();
}

/// A server function a stale part of the page still runs, after the page was
/// drawn again and its context went: an error, not a panic that would keep
/// the response open forever.
#[tokio::test]
async fn a_server_function_without_its_context_fails_without_a_panic() {
    assert!(strata_admin_ui::api::get_settings().await.is_err());
    assert!(strata_admin_ui::api::get_setup().await.is_err());
}
