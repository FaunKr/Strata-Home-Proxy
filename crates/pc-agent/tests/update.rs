//! Updating the service from the web UI, with a fake NAS serving the program.

mod common;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing};
use common::*;
use strata_pc_agent::strata;

const PROGRAM: &[u8] = b"MZ new program";

/// What the fake NAS answers for the program: status and body.
type Served = Arc<Mutex<(StatusCode, Vec<u8>)>>;

async fn program(State(served): State<Served>) -> impl IntoResponse {
    served.lock().unwrap().clone()
}

async fn nas(status: StatusCode, body: &[u8]) -> (SocketAddr, Served) {
    let served: Served = Arc::new(Mutex::new((status, body.to_vec())));
    let addr = serve(
        Router::new()
            .route("/admin/agent/strata-pc-agent.exe", routing::get(program))
            .with_state(served.clone()),
    )
    .await;
    (addr, served)
}

async fn with_nas(name: &str, status: StatusCode, body: &[u8]) -> Setup {
    let (addr, _) = nas(status, body).await;
    setup_with(name, |c| c.nas_url = Some(format!("http://{addr}"))).await
}

/// Nothing changed: the old program runs on, no file left behind.
fn unchanged(s: &Setup) {
    assert!(s.fake.replaced.lock().unwrap().is_none());
    assert_eq!(s.fake.exits.load(Ordering::SeqCst), 0);
    assert!(!s.dir.join("strata-pc-agent.exe.neu").exists());
    assert!(!marker(s).exists());
}

/// What the old service leaves for the new one when it stopped a running Strata.
fn marker(s: &Setup) -> std::path::PathBuf {
    s.dir.join("strata-after-update")
}

#[tokio::test]
async fn status_reports_the_version() {
    let s = setup("update-version").await;
    let status = get(&s, "/status").await;
    assert_eq!(status["version"], strata_pc_agent::VERSION);
    assert!(strata_pc_agent::VERSION.starts_with(env!("CARGO_PKG_VERSION")));
}

/// The NAS reads the version from the .exe it serves, without running it:
/// the first marker followed by a version (`version_in` in
/// `nas-proxy/src/pc_setup.rs`). The Dockerfile checks the image's .exe the
/// same way.
#[test]
fn the_program_carries_its_version_as_text() {
    let program = std::fs::read(env!("CARGO_BIN_EXE_strata-pc-agent")).unwrap();
    let marker = b"strata-pc-agent-version:";
    let found = program
        .windows(marker.len())
        .enumerate()
        .filter(|(_, w)| w == marker)
        .find_map(|(at, _)| {
            let rest = &program[at + marker.len()..];
            let end = rest.iter().take(64).position(|b| *b == b'\n')?;
            let version = std::str::from_utf8(&rest[..end]).ok()?;
            (!version.is_empty() && version.bytes().all(|b| b.is_ascii_graphic()))
                .then_some(version)
        });
    assert_eq!(found, Some(strata_pc_agent::VERSION));
}

#[tokio::test]
async fn update_puts_the_program_from_the_nas_in_place_and_ends_the_service() {
    let s = with_nas("update-ok", StatusCode::OK, PROGRAM).await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 202, "{body}");
    assert_eq!(s.fake.replaced.lock().unwrap().as_deref(), Some(PROGRAM));
    // The answer goes out first; the service ends a moment later.
    assert_eq!(s.fake.exits.load(Ordering::SeqCst), 0);
    tokio::time::sleep(strata_pc_agent::update::EXIT_DELAY + Duration::from_millis(500)).await;
    assert_eq!(s.fake.exits.load(Ordering::SeqCst), 1);
    // A second press while the first one hands over does nothing.
    let (code, _) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 409);
}

#[tokio::test]
async fn failed_download_leaves_everything_as_it_was() {
    let s = with_nas("update-404", StatusCode::NOT_FOUND, b"").await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 500);
    assert!(body["reason"].as_str().unwrap().contains("404"), "{body}");
    tokio::time::sleep(strata_pc_agent::update::EXIT_DELAY).await;
    unchanged(&s);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0, "Strata stays up");
}

#[tokio::test]
async fn unreachable_nas_leaves_everything_as_it_was() {
    let s = setup_with("update-unreachable", |c| {
        c.nas_url = Some("http://127.0.0.1:9".into())
    })
    .await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 500);
    assert!(
        body["reason"].as_str().unwrap().contains("Download"),
        "{body}"
    );
    unchanged(&s);
}

#[tokio::test]
async fn something_other_than_a_windows_program_is_refused() {
    let s = with_nas("update-html", StatusCode::OK, b"<html>").await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 500);
    assert!(
        body["reason"].as_str().unwrap().contains("Windows program"),
        "{body}"
    );
    unchanged(&s);
}

#[tokio::test]
async fn failing_to_put_the_program_in_place_keeps_the_service_running() {
    let s = with_nas("update-locked", StatusCode::OK, PROGRAM).await;
    *s.fake.replace_fails.lock().unwrap() = Some("Access denied".into());
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 500);
    assert!(
        body["reason"].as_str().unwrap().contains("Access denied"),
        "{body}"
    );
    tokio::time::sleep(strata_pc_agent::update::EXIT_DELAY).await;
    unchanged(&s);
    // The old service brings back the Strata it stopped.
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert!(s.fake.strata_up.load(Ordering::SeqCst));
    // The next try may go ahead.
    *s.fake.replace_fails.lock().unwrap() = None;
    assert_eq!(post(&s, "/update", serde_json::json!({})).await.0, 202);
}

#[tokio::test]
async fn update_waits_while_strata_works() {
    let s = with_nas("update-busy", StatusCode::OK, PROGRAM).await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 409);
    assert!(
        body["reason"].as_str().unwrap().contains("Strata"),
        "{body}"
    );
    unchanged(&s);
}

#[tokio::test]
async fn without_the_nas_address_there_is_no_update() {
    let s = setup("update-no-nas").await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 409);
    assert!(
        body["reason"].as_str().unwrap().contains("install command"),
        "{body}"
    );
    unchanged(&s);
}

/// A NAS that takes a while to send the program.
async fn slow_nas() -> SocketAddr {
    async fn slow() -> impl IntoResponse {
        tokio::time::sleep(Duration::from_millis(800)).await;
        PROGRAM.to_vec()
    }
    serve(Router::new().route("/admin/agent/strata-pc-agent.exe", routing::get(slow))).await
}

#[tokio::test]
async fn no_job_starts_and_no_sleep_while_an_update_loads() {
    let addr = slow_nas().await;
    let s = setup_with("update-slow", |c| {
        c.nas_url = Some(format!("http://{addr}"))
    })
    .await;
    let url = s.url.clone();
    let update = tokio::spawn(async move {
        client()
            .post(format!("{url}/update"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (code, body) = post(&s, "/strata/jobs", serde_json::json!({"kind": "benchmark"})).await;
    assert_eq!(code, 409, "{body}");
    assert!(
        body["reason"].as_str().unwrap().contains("updating"),
        "{body}"
    );
    let (code, body) = sleep_answer(&s).await;
    assert_eq!(code, 409, "{body}");
    assert_eq!(update.await.unwrap(), 202);
}

#[tokio::test]
async fn neither_forced_sleep_nor_forced_restart_while_an_update_loads() {
    let addr = slower_nas().await;
    let s = setup_with("update-force", |c| {
        c.nas_url = Some(format!("http://{addr}"))
    })
    .await;
    let update = start_update(&s);
    tokio::time::sleep(Duration::from_millis(300)).await;
    for path in ["/sleep/force", "/strata/kill"] {
        let (code, body) = post(&s, path, serde_json::json!({})).await;
        assert_eq!(code, 409, "{path}: {body}");
        assert_eq!(body["reason"], "The PC service is updating", "{path}");
    }
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
    assert_eq!(update.await.unwrap(), 202);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 0);
}

/// A fake NAS that takes 3 s for the program, well longer than the checks.
async fn slower_nas() -> SocketAddr {
    async fn slower() -> impl IntoResponse {
        tokio::time::sleep(Duration::from_secs(3)).await;
        PROGRAM.to_vec()
    }
    serve(Router::new().route("/admin/agent/strata-pc-agent.exe", routing::get(slower))).await
}

/// Starts an update in the background; its HTTP status.
fn start_update(s: &Setup) -> tokio::task::JoinHandle<u16> {
    let url = s.url.clone();
    tokio::spawn(async move {
        client()
            .post(format!("{url}/update"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    })
}

#[tokio::test]
async fn a_forced_restart_starts_no_strata_once_an_update_began_during_its_pause() {
    let addr = slower_nas().await;
    let s = setup_with("update-force-restart-pause", |c| {
        c.nas_url = Some(format!("http://{addr}"));
        c.stop_settle_ms = 1500;
    })
    .await;
    let url = s.url.clone();
    let kill = tokio::spawn(async move {
        let resp = client()
            .post(format!("{url}/strata/kill"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        (
            resp.status().as_u16(),
            resp.json::<serde_json::Value>().await.unwrap(),
        )
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let update = start_update(&s);
    let (code, body) = kill.await.unwrap();
    assert_eq!(code, 409, "{body}");
    assert!(
        body["reason"].as_str().unwrap().contains("update"),
        "{body}"
    );
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(update.await.unwrap(), 202);
}

#[tokio::test]
async fn no_standby_once_an_update_began_after_the_sleep_was_accepted() {
    let addr = slower_nas().await;
    let s = setup_with("update-after-sleep", |c| {
        c.nas_url = Some(format!("http://{addr}"))
    })
    .await;
    let (code, body) = post(&s, "/sleep", serde_json::json!({})).await;
    assert_eq!(code, 200, "{body}");
    let update = start_update(&s);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 0);
    assert_eq!(update.await.unwrap(), 202);
}

// --- #214 Strata stops before the handover and starts with the new service ---

#[tokio::test]
async fn update_stops_strata_before_the_handover_and_leaves_a_marker() {
    let s = with_nas("update-stops-strata", StatusCode::OK, PROGRAM).await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 202, "{body}");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        *s.fake.strata_up_at_replace.lock().unwrap(),
        Some(false),
        "Strata ended before the program was swapped"
    );
    assert!(marker(&s).exists());
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_stopped_strata_gets_no_marker() {
    let s = with_nas("update-strata-off", StatusCode::OK, PROGRAM).await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 202, "{body}");
    assert!(s.fake.replaced.lock().unwrap().is_some());
    assert!(!marker(&s).exists());
}

#[tokio::test]
async fn a_strata_that_keeps_answering_refuses_the_update() {
    let s = with_nas("update-strata-stays", StatusCode::OK, PROGRAM).await;
    s.fake.strata_ignores_stop.store(true, Ordering::SeqCst);
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 409, "{body}");
    assert!(
        body["reason"].as_str().unwrap().contains("did not stop"),
        "{body}"
    );
    tokio::time::sleep(strata_pc_agent::update::EXIT_DELAY).await;
    unchanged(&s);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        0,
        "no second Strata next to the one still answering"
    );
    // The next try may go ahead.
    s.fake.strata_ignores_stop.store(false, Ordering::SeqCst);
    assert_eq!(post(&s, "/update", serde_json::json!({})).await.0, 202);
}

#[tokio::test]
async fn nothing_in_the_old_service_starts_strata_after_the_handover() {
    let (addr, _) = nas(StatusCode::OK, PROGRAM).await;
    let s = setup_with("update-no-start", |c| {
        c.nas_url = Some(format!("http://{addr}"));
        c.fresh_strata_after_standby = true;
    })
    .await;
    let (code, body) = post(&s, "/update", serde_json::json!({})).await;
    assert_eq!(code, 202, "{body}");
    s.agent.watchdog_tick().await;
    s.agent.resumed();
    s.agent.set_gaming(true);
    s.agent.set_gaming(false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

/// The new service's start on the folders the old one left.
async fn new_service_starts(s: &Setup) {
    strata::manage_configs_at_start(&s.restarted()).await.await;
}

#[tokio::test]
async fn the_new_service_starts_strata_once_and_removes_the_marker() {
    let s = setup("update-marker-start").await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    std::fs::write(marker(&s), b"").unwrap();
    new_service_starts(&s).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert!(!marker(&s).exists());
    new_service_starts(&s).await;
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        1,
        "only after an update"
    );
}

#[tokio::test]
async fn the_marker_starts_no_strata_in_gaming_mode() {
    let s = setup("update-marker-gaming").await;
    s.agent.set_gaming(true);
    std::fs::write(marker(&s), b"").unwrap();
    new_service_starts(&s).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert!(!marker(&s).exists());
}

#[tokio::test]
async fn the_marker_starts_no_strata_without_auto_start() {
    let s = setup_with("update-marker-no-auto", |c| c.auto_start = false).await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    std::fs::write(marker(&s), b"").unwrap();
    new_service_starts(&s).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert!(!marker(&s).exists());
}

#[tokio::test]
async fn the_marker_starts_no_strata_without_a_start_script() {
    let s = setup_with("update-marker-no-script", |c| c.strata_start = None).await;
    s.fake.strata_up.store(false, Ordering::SeqCst);
    std::fs::write(marker(&s), b"").unwrap();
    new_service_starts(&s).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert!(!marker(&s).exists());
}
