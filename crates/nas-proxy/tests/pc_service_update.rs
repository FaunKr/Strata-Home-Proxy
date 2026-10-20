//! The PC service's version on the overview and the update button. The
//! last version survives a restart; an update wakes a sleeping PC.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use strata_nas_proxy::events::EventKind;

const OLD: &str = "0.1.0+alt1234";
const NEW: &str = "0.1.0+neu5678";

/// A stand-in for the image's `strata-pc-agent.exe` that carries `version`
/// as the real program does, with the marker's prefix on its own before it
/// (the compiler may put other text right after such a copy).
fn program(name: &str, version: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("strata-proxy-update-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("strata-pc-agent.exe");
    let mut bytes = b"MZ\x90\x00 code strata-pc-agent-version:called `Result::unwrap()`\n".to_vec();
    bytes.extend_from_slice(format!("strata-pc-agent-version:{version}\n").as_bytes());
    bytes.extend_from_slice(b"\x00 more code");
    std::fs::write(&exe, bytes).unwrap();
    exe
}

/// An awake PC whose service runs `OLD`, with `NEW` in the image.
async fn outdated(name: &str) -> Setup {
    let s = setup(true, |_| {}).await;
    s.state.pc_setup.set_program(program(name, NEW));
    *s.pc.version.lock().unwrap() = Some(OLD.into());
    s
}

#[tokio::test]
async fn overview_shows_the_version_and_whether_an_update_is_available() {
    let s = outdated("available").await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    assert_eq!(v["pc_service"]["update_available"], true);
    assert_eq!(v["pc_service"]["available"], NEW);

    *s.pc.version.lock().unwrap() = Some(NEW.into());
    let v = live
        .until("same version", |v| v["pc_service"]["version"] == NEW)
        .await;
    assert_eq!(v["pc_service"]["update_available"], false);
}

#[tokio::test]
async fn a_service_without_version_counts_as_outdated() {
    let s = outdated("no-version").await;
    *s.pc.version.lock().unwrap() = None;
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("PC service", |v| !v["pc_service"].is_null())
        .await;
    assert!(v["pc_service"]["version"].is_null());
    assert_eq!(v["pc_service"]["update_available"], true);
}

#[tokio::test]
async fn without_a_program_in_the_image_there_is_no_update() {
    let s = outdated("no-program").await;
    s.state
        .pc_setup
        .set_program(std::env::temp_dir().join("does-not-exist/strata-pc-agent.exe"));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    let v = live
        .until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    assert_eq!(v["pc_service"]["update_available"], false);
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 400);
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_sleeping_pc_keeps_its_last_version_and_is_woken_for_an_update() {
    let s = outdated("asleep").await;
    *s.pc.update_to.lock().unwrap() = Some(NEW.into());
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let v = live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["pc_service"]["version"], OLD);
    assert_eq!(v["pc_service"]["update_available"], true);

    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 1);
    live.until("new version", |v| v["pc_service"]["version"] == NEW)
        .await;
    let kinds = kinds(&s.state);
    let requested = kinds
        .iter()
        .position(|k| *k == EventKind::PcServiceUpdateRequested)
        .expect("update in the history");
    let woken = kinds
        .iter()
        .position(|k| *k == EventKind::WakeRequested)
        .expect("wake in the history");
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::PcServiceUpdateRequested)
            .count(),
        1
    );
    // The request comes before the wake it caused.
    assert!(requested < woken, "{kinds:?}");
    let events = s.state.events.recent();
    let detail = |kind| {
        events
            .iter()
            .find(|e| e.kind == kind)
            .and_then(|e| e.detail.clone())
    };
    assert_eq!(
        detail(EventKind::PcServiceUpdateRequested).as_deref(),
        Some("from the web UI")
    );
    assert_eq!(
        detail(EventKind::WakeRequested).as_deref(),
        Some("for the PC service update, from the web UI")
    );
}

#[tokio::test]
async fn a_press_while_the_service_restarts_for_an_update_does_not_wake() {
    let s = outdated("restarting").await;
    s.state.pc_setup.set_update_timeout(Duration::from_secs(60));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    // The service restarts with the new version and answers nothing meanwhile.
    s.pc.awake.store(false, Ordering::SeqCst);
    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("running already"));
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    let requests = kinds(&s.state)
        .into_iter()
        .filter(|k| *k == EventKind::PcServiceUpdateRequested)
        .count();
    assert_eq!(requests, 1);
}

#[tokio::test]
async fn a_service_updated_by_hand_while_asleep_is_not_updated_after_the_wake() {
    let s = outdated("by-hand").await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    s.pc.awake.store(false, Ordering::SeqCst);
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    // Set up again with the command, say, while the proxy did not look.
    *s.pc.version.lock().unwrap() = Some(NEW.into());

    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 0);
    let v = live
        .until("current version", |v| v["pc_service"]["version"] == NEW)
        .await;
    assert_eq!(v["pc_service"]["update_available"], false);
    assert_eq!(v["pc_service"]["updating"], false);
    assert!(v["pc_service"]["update_error"].is_null());
}

#[tokio::test]
async fn a_pc_that_does_not_wake_gets_no_update() {
    let s = setup(true, |c| c.wake_timeout = Duration::from_millis(800)).await;
    s.state.pc_setup.set_program(program("no-wake", NEW));
    *s.pc.version.lock().unwrap() = Some(OLD.into());
    s.pc.ignore_wol.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    s.pc.awake.store(false, Ordering::SeqCst);
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;

    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("did not wake"));
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 0);
    let v = live.until("still asleep", |v| v["pc"] == "Asleep").await;
    assert_eq!(v["pc_service"]["updating"], false);
}

#[tokio::test]
async fn without_a_mac_a_sleeping_pc_gets_no_update() {
    let s = setup(false, |c| c.pc_mac = None).await;
    s.state.pc_setup.set_program(program("no-mac", NEW));
    let mut b = logged_in(&s.url).await;
    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("No MAC address"));
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 0);
    assert!(!kinds(&s.state).contains(&EventKind::PcServiceUpdateRequested));
}

#[tokio::test]
async fn without_a_strata_address_a_sleeping_pc_gets_no_update() {
    let s = setup(false, |c| c.strata_url = None).await;
    s.state.pc_setup.set_program(program("no-strata", NEW));
    let mut b = logged_in(&s.url).await;
    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("Strata address is missing")
    );
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert!(!kinds(&s.state).contains(&EventKind::PcServiceUpdateRequested));
}

/// The proxy of `first` started again with the image carrying `NEW` and the
/// PC asleep, not asked yet; a browser logged in to it.
async fn after_restart(first: &Setup, name: &str) -> Browser {
    first.pc.awake.store(false, Ordering::SeqCst);
    let state = restarted(first);
    state.pc_setup.set_program(program(name, NEW));
    logged_in_again(&serve_proxy(state).await).await
}

#[tokio::test]
async fn the_last_version_survives_a_restart() {
    let dir = data_dir("restart");
    let first = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    first.state.pc_setup.set_program(program("restart", NEW));
    *first.pc.version.lock().unwrap() = Some(OLD.into());
    let b = logged_in(&first.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    // The saved version follows the reported one.
    *first.pc.version.lock().unwrap() = Some("0.1.0+mitte".into());
    live.until("other version", |v| {
        v["pc_service"]["version"] == "0.1.0+mitte"
    })
    .await;

    let b = after_restart(&first, "restart").await;
    let v = Live::open(&b)
        .await
        .until("saved version", |v| !v["pc_service"].is_null())
        .await;
    assert_eq!(v["pc_service"]["version"], "0.1.0+mitte");
    assert_eq!(v["pc_service"]["update_available"], true);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_service_without_version_stays_outdated_after_a_restart() {
    let dir = data_dir("no-version");
    let first = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    first
        .state
        .pc_setup
        .set_program(program("restart-none", NEW));
    let b = logged_in(&first.url).await;
    Live::open(&b)
        .await
        .until("reported", |v| !v["pc_service"].is_null())
        .await;

    let b = after_restart(&first, "restart-none").await;
    let v = Live::open(&b)
        .await
        .until("saved version", |v| !v["pc_service"].is_null())
        .await;
    assert!(v["pc_service"]["version"].is_null());
    assert_eq!(v["pc_service"]["update_available"], true);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn update_button_updates_the_service_and_records_it() {
    let s = outdated("ok").await;
    *s.pc.update_to.lock().unwrap() = Some(NEW.into());
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 1);
    let v = live
        .until("new version", |v| v["pc_service"]["version"] == NEW)
        .await;
    assert_eq!(v["pc_service"]["update_available"], false);
    assert_eq!(v["pc_service"]["updating"], false);
    assert!(v["pc_service"]["update_error"].is_null());
    let events = s.state.events.recent();
    let requested = events
        .iter()
        .find(|e| e.kind == EventKind::PcServiceUpdateRequested)
        .expect("update in the history");
    assert_eq!(requested.detail.as_deref(), Some("from the web UI"));
    let updated = events
        .iter()
        .find(|e| e.kind == EventKind::PcServiceUpdated)
        .expect("new version in the history");
    assert_eq!(updated.detail.as_deref(), Some(NEW));
}

#[tokio::test]
async fn a_refused_update_shows_its_reason() {
    let s = outdated("refused").await;
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is working on a request".into());
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    b.call("pc-service-update", &[]).await;
    let v = live
        .until("reason", |v| !v["pc_service"]["update_error"].is_null())
        .await;
    assert!(
        v["pc_service"]["update_error"]
            .as_str()
            .unwrap()
            .contains("Strata is working")
    );
    assert_eq!(v["pc_service"]["update_available"], true);
    assert!(kinds(&s.state).contains(&EventKind::PcServiceUpdateFailed));
}

#[tokio::test]
async fn an_old_service_without_update_is_told_to_set_up_again() {
    let s = outdated("old").await;
    s.pc.no_update.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    b.call("pc-service-update", &[]).await;
    let v = live
        .until("reason", |v| !v["pc_service"]["update_error"].is_null())
        .await;
    assert!(
        v["pc_service"]["update_error"]
            .as_str()
            .unwrap()
            .contains("Install it once more")
    );
}

#[tokio::test]
async fn an_update_that_never_reports_back_fails_after_a_while() {
    let s = outdated("silent").await;
    s.state
        .pc_setup
        .set_update_timeout(Duration::from_millis(500));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    b.call("pc-service-update", &[]).await;
    live.until("update running", |v| v["pc_service"]["updating"] == true)
        .await;
    let v = live
        .until("error", |v| !v["pc_service"]["update_error"].is_null())
        .await;
    assert_eq!(v["pc_service"]["updating"], false);
    assert!(kinds(&s.state).contains(&EventKind::PcServiceUpdateFailed));
}

#[tokio::test]
async fn a_second_press_while_an_update_runs_changes_nothing() {
    let s = outdated("twice").await;
    s.state.pc_setup.set_update_timeout(Duration::from_secs(60));
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("running already"));
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 1);
    let v = live
        .until("update running", |v| v["pc_service"]["updating"] == true)
        .await;
    assert!(v["pc_service"]["update_error"].is_null());

    // The new version arrives: counted as done, no error from the second press.
    *s.pc.version.lock().unwrap() = Some(NEW.into());
    let v = live
        .until("new version", |v| v["pc_service"]["version"] == NEW)
        .await;
    assert_eq!(v["pc_service"]["updating"], false);
    assert!(v["pc_service"]["update_error"].is_null());
    assert!(kinds(&s.state).contains(&EventKind::PcServiceUpdated));
}

#[tokio::test]
async fn an_up_to_date_service_is_not_updated() {
    let s = outdated("current").await;
    *s.pc.version.lock().unwrap() = Some(NEW.into());
    let mut b = logged_in(&s.url).await;
    let resp = b.call("pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("already"));
    assert_eq!(s.pc.update_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failure_clears_once_the_service_runs_the_new_version() {
    let s = outdated("clears").await;
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is working on a request".into());
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    b.call("pc-service-update", &[]).await;
    live.until("reason", |v| !v["pc_service"]["update_error"].is_null())
        .await;
    // Set up again with the command, say: the right version runs.
    *s.pc.version.lock().unwrap() = Some(NEW.into());
    let v = live
        .until("no error", |v| {
            v["pc_service"]["version"] == NEW && v["pc_service"]["update_error"].is_null()
        })
        .await;
    assert_eq!(v["pc_service"]["update_available"], false);
}

/// The page that pressed the button may close before the PC service
/// answers. The update goes on, and its answer still reaches the overview.
#[tokio::test]
async fn an_update_whose_page_closes_early_still_records_the_answer() {
    let s = outdated("page-closes").await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(3);
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is working on a request".into());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("Version", |v| v["pc_service"]["version"] == OLD)
        .await;
    let closed = reqwest::Client::new()
        .post(format!("{}/admin/api/pc-service-update", b.base))
        .header("cookie", b.cookie.clone().unwrap())
        .timeout(Duration::from_secs(1))
        .send()
        .await;
    assert!(closed.is_err(), "the page closes before the answer");
    let v = live
        .until("reason", |v| !v["pc_service"]["update_error"].is_null())
        .await;
    assert!(
        v["pc_service"]["update_error"]
            .as_str()
            .unwrap()
            .contains("Strata is working")
    );
}
