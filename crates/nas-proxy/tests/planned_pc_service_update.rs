//! The Planned PC service update: *Update before next sleep* turns the
//! proxy's next sleep request after the idle timeout into `POST /update`,
//! with the sleep request right after the new version answers.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_admin_ui::api::PlannedUpdateView;
use strata_nas_proxy::events::EventKind;

const OLD: &str = "0.1.0";
const NEW: &str = "0.2.0";

/// The idle timeout once a test lets it run out; until then an hour, so a
/// slow login under load never runs into it.
const IDLE_SECS: &str = "1";

/// An awake PC whose service runs `OLD` and updates to `NEW`, the program
/// in the image, with the overview open and offering the update.
async fn ready(
    name: &str,
    tweak: impl FnOnce(&mut strata_nas_proxy::Config),
) -> (Setup, Browser, Live) {
    let s = setup(true, tweak).await;
    s.state.pc_setup.set_program(agent_program(name, NEW));
    *s.pc.version.lock().unwrap() = Some(OLD.into());
    *s.pc.update_to.lock().unwrap() = Some(NEW.into());
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("update offered", |v| {
        v["pc"] == "Awake"
            && v["pc_service"]["update_available"] == true
            && v["pc_service"]["planned"] == "Off"
    })
    .await;
    (s, b, live)
}

/// Lets the idle timeout run out `secs` from now.
fn go_idle(s: &Setup, secs: &str) {
    s.state.settings.set("IDLE_TIMEOUT_SECS", secs).unwrap();
    s.state.touch();
}

/// Arms the Planned PC service update.
async fn plan(b: &mut Browser) {
    let resp = b.call("plan-pc-service-update", &[]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

/// Arms the Planned PC service update and lets the idle timeout run out.
async fn arm(s: &Setup, b: &mut Browser) {
    plan(b).await;
    go_idle(s, IDLE_SECS);
}

fn planned(v: &Value) -> bool {
    v["pc_service"]["planned"]["Planned"].is_object()
}

fn updates(s: &Setup) -> usize {
    s.pc.update_requests.load(Ordering::SeqCst)
}

fn sleeps(s: &Setup) -> usize {
    s.pc.sleep_requests.load(Ordering::SeqCst)
}

fn count(s: &Setup, kind: EventKind) -> usize {
    kinds(&s.state).into_iter().filter(|k| *k == kind).count()
}

/// The detail of the latest event of `kind`.
fn detail(s: &Setup, kind: EventKind) -> String {
    s.state
        .events
        .recent()
        .into_iter()
        .rev()
        .find(|e| e.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {:?}", kinds(&s.state)))
        .detail
        .unwrap_or_default()
}

async fn update_sent(s: &Setup) {
    eventually("update request", || updates(s) == 1).await;
}

async fn sleep_sent(s: &Setup) {
    eventually("sleep request", || sleeps(s) == 1).await;
}

#[tokio::test]
async fn idle_timeout_updates_a_pc_service_with_a_planned_update() {
    let (s, mut b, _live) = ready("idle-updates", |_| {}).await;
    arm(&s, &mut b).await;
    update_sent(&s).await;
    assert_eq!(
        detail(&s, EventKind::PcServiceUpdateRequested),
        "before next sleep"
    );
    assert_eq!(
        detail(&s, EventKind::PcServiceUpdatePlanned),
        "from the web UI"
    );
}

#[tokio::test]
async fn the_pc_sleeps_right_after_the_planned_update() {
    let (s, mut b, _live) = ready("sleeps-after", |_| {}).await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(2);
    arm(&s, &mut b).await;
    update_sent(&s).await;
    // While the update runs, only the planned update's own sleep can come in
    // time: the idle timeout no longer holds.
    s.state.settings.set("IDLE_TIMEOUT_SECS", "3600").unwrap();
    sleep_sent(&s).await;
    eventually("sleep in the history", || {
        kinds(&s.state).contains(&EventKind::SleepAccepted)
    })
    .await;
    let kinds = own_kinds(&s.state);
    let updated = kinds
        .iter()
        .position(|k| *k == EventKind::PcServiceUpdated)
        .expect("new version in the history");
    let asleep = kinds
        .iter()
        .position(|k| *k == EventKind::SleepAccepted)
        .expect("sleep in the history");
    assert!(updated < asleep, "{kinds:?}");
}

#[tokio::test]
async fn a_missed_status_answer_does_not_delay_the_sleep_after_the_planned_update() {
    let (s, mut b, mut live) = ready("missed-status", |_| {}).await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(2);
    arm(&s, &mut b).await;
    update_sent(&s).await;
    // The PC service restarts with the update: its status misses once and
    // Strata is not back yet, so the miss reads as a sleeping PC. The next
    // status marks it awake again and starts the idle time.
    s.pc.status_misses.store(1, Ordering::SeqCst);
    s.pc.starting.store(true, Ordering::SeqCst);
    s.state.settings.set("IDLE_TIMEOUT_SECS", "3600").unwrap();
    live.until("new version, plan over", |v| {
        v["pc_service"]["version"] == NEW && v["pc_service"]["planned"] == "Off"
    })
    .await;
    sleep_sent(&s).await;
}

#[tokio::test]
async fn no_sleep_request_while_the_planned_update_runs() {
    let (s, mut b, _live) = ready("no-sleep-meanwhile", |_| {}).await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(2);
    arm(&s, &mut b).await;
    update_sent(&s).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(sleeps(&s), 0);
    sleep_sent(&s).await;
}

#[tokio::test]
async fn the_overview_says_updating_while_the_planned_update_runs() {
    let (s, mut b, mut live) = ready("overview-running", |_| {}).await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(2);
    arm(&s, &mut b).await;
    live.until("running", |v| v["pc_service"]["planned"] == "Running")
        .await;
}

#[tokio::test]
async fn idle_timeout_sleeps_a_pc_without_a_planned_update() {
    let (s, _b, _live) = ready("not-armed", |_| {}).await;
    go_idle(&s, IDLE_SECS);
    sleep_sent(&s).await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn a_planned_update_ends_once_the_new_version_reports() {
    let (s, mut b, mut live) = ready("ends", |_| {}).await;
    arm(&s, &mut b).await;
    live.until("new version, plan over", |v| {
        v["pc_service"]["version"] == NEW && v["pc_service"]["planned"] == "Off"
    })
    .await;
    sleep_sent(&s).await;
    assert_eq!(updates(&s), 1);
    assert_eq!(count(&s, EventKind::PcServiceUpdatePlanCancelled), 0);
}

#[tokio::test]
async fn a_planned_update_can_be_armed_while_the_pc_sleeps() {
    let (s, mut b, mut live) = ready("asleep", |_| {}).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    live.until("asleep", |v| v["pc"] == "Asleep").await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancel_ends_a_planned_update() {
    let (s, mut b, mut live) = ready("cancel", |_| {}).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    let resp = b.call("cancel-pc-service-update", &[]).await;
    assert_eq!(resp.status(), 200);
    live.until("off", |v| v["pc_service"]["planned"] == "Off")
        .await;
    assert_eq!(
        detail(&s, EventKind::PcServiceUpdatePlanCancelled),
        "from the web UI"
    );
    go_idle(&s, IDLE_SECS);
    sleep_sent(&s).await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn a_planned_update_survives_a_proxy_restart() {
    let dir = data_dir("planned-update");
    let (s, mut b, mut live) = ready("survives", |c| c.data_dir = Some(dir.clone())).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    drop(live);

    let again = restarted(&s);
    again.pc_setup.set_program(agent_program("survives", NEW));
    let planned = again.pc_setup.view().0.expect("saved version").planned;
    assert!(
        matches!(planned, PlannedUpdateView::Planned { .. }),
        "{planned:?}"
    );
    // The idle timeout of the restarted proxy runs out at once.
    again.settings.set("IDLE_TIMEOUT_SECS", "0").unwrap();
    again.idle_tick().await;
    update_sent(&s).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_planned_update_ends_silently_when_the_service_runs_the_new_version() {
    let (s, mut b, mut live) = ready("silently", |_| {}).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    *s.pc.version.lock().unwrap() = Some(NEW.into());
    live.until("off", |v| v["pc_service"]["planned"] == "Off")
        .await;
    assert_eq!(count(&s, EventKind::PcServiceUpdatePlanCancelled), 0);
    go_idle(&s, IDLE_SECS);
    sleep_sent(&s).await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn the_update_button_cancels_a_planned_update() {
    let (s, mut b, mut live) = ready("by-hand", |_| {}).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    assert_eq!(
        detail(&s, EventKind::PcServiceUpdatePlanCancelled),
        "updated by hand"
    );
    live.until("off", |v| v["pc_service"]["planned"] == "Off")
        .await;
}

#[tokio::test]
async fn a_refused_planned_update_keeps_the_plan_and_shows_the_reason() {
    let (s, mut b, mut live) = ready("refused-reason", |_| {}).await;
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is processing a request".into());
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    let v = live
        .until("refusal shown", |v| {
            v["pc_service"]["planned"]["Planned"]["refused"].is_string()
        })
        .await;
    assert_eq!(
        v["pc_service"]["planned"]["Planned"]["refused"],
        "Strata is processing a request"
    );
    assert!(v["pc_service"]["update_error"].is_null(), "{v}");
}

#[tokio::test]
async fn a_refused_planned_update_is_followed_by_a_sleep_request() {
    let (s, mut b, _live) = ready("refused-sleeps", |_| {}).await;
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is processing a request".into());
    arm(&s, &mut b).await;
    sleep_sent(&s).await;
    assert_eq!(updates(&s), 1);
}

#[tokio::test]
async fn a_refused_planned_update_is_tried_again_after_the_sleep_retry() {
    let (s, mut b, _live) = ready("refused-retry", |c| {
        c.sleep_retry = Duration::from_millis(300)
    })
    .await;
    *s.pc.refuse_update.lock().unwrap() = Some("Strata is processing a request".into());
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    eventually("second update request", || updates(&s) >= 2).await;
    assert!(sleeps(&s) >= 1);
}

#[tokio::test]
async fn a_failed_planned_update_ends_the_plan_and_the_pc_sleeps() {
    let (s, mut b, mut live) = ready("failed", |_| {}).await;
    *s.pc.fail_update.lock().unwrap() = Some("The download failed".into());
    arm(&s, &mut b).await;
    let v = live
        .until("failure, plan over", |v| {
            v["pc_service"]["update_error"].is_string() && v["pc_service"]["planned"] == "Off"
        })
        .await;
    assert!(
        v["pc_service"]["update_error"]
            .as_str()
            .unwrap()
            .contains("The download failed")
    );
    sleep_sent(&s).await;
}

#[tokio::test]
async fn a_planned_update_too_old_for_the_service_ends_the_plan_and_the_pc_sleeps() {
    let (s, mut b, mut live) = ready("too-old", |_| {}).await;
    s.pc.no_update.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    let v = live
        .until("failure, plan over", |v| {
            v["pc_service"]["update_error"].is_string() && v["pc_service"]["planned"] == "Off"
        })
        .await;
    assert!(
        v["pc_service"]["update_error"]
            .as_str()
            .unwrap()
            .contains("Install it once more")
    );
    sleep_sent(&s).await;
}

#[tokio::test]
async fn a_planned_update_that_never_reports_back_ends_the_plan_and_the_pc_sleeps() {
    let (s, mut b, mut live) = ready("never-back", |_| {}).await;
    *s.pc.update_to.lock().unwrap() = None;
    s.state
        .pc_setup
        .set_update_timeout(Duration::from_millis(500));
    arm(&s, &mut b).await;
    live.until("failure, plan over", |v| {
        v["pc_service"]["update_error"].is_string() && v["pc_service"]["planned"] == "Off"
    })
    .await;
    sleep_sent(&s).await;
    assert!(kinds(&s.state).contains(&EventKind::PcServiceUpdateFailed));
}

#[tokio::test]
async fn a_request_during_the_planned_update_puts_off_the_sleep() {
    let (s, mut b, mut live) = ready("request-meanwhile", |_| {}).await;
    *s.pc.update_delay.lock().unwrap() = Duration::from_secs(2);
    arm(&s, &mut b).await;
    update_sent(&s).await;
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    // Long enough that only an immediate sleep could still come.
    s.state.settings.set("IDLE_TIMEOUT_SECS", "30").unwrap();
    live.until("new version", |v| v["pc_service"]["version"] == NEW)
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(sleeps(&s), 0);
}

#[tokio::test]
async fn keep_awake_holds_a_planned_update() {
    let (s, mut b, _live) = ready("keep-awake", |_| {}).await;
    let resp = b.call("keep-awake", &[("choice", "1h")]).await;
    assert_eq!(resp.status(), 200);
    arm(&s, &mut b).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn force_sleep_leaves_a_planned_update_armed() {
    let (s, mut b, mut live) = ready("force-sleep", |_| {}).await;
    plan(&mut b).await;
    let resp = b.call("force-sleep", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.force_sleeps.load(Ordering::SeqCst), 1);
    live.until("asleep and planned", |v| v["pc"] == "Asleep" && planned(v))
        .await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn sleep_now_leaves_a_planned_update_armed() {
    let (s, mut b, mut live) = ready("sleep-now", |_| {}).await;
    plan(&mut b).await;
    let resp = b.call("sleep-now", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(sleeps(&s), 1);
    live.until("asleep and planned", |v| v["pc"] == "Asleep" && planned(v))
        .await;
    assert_eq!(updates(&s), 0);
}

#[tokio::test]
async fn a_planned_restart_follows_the_planned_update() {
    let (s, mut b, mut live) = ready("then-restart", |_| {}).await;
    s.pc.takes_restart.store(true, Ordering::SeqCst);
    // The proxy needs one answer with the service's new capability first.
    live.until("restart offered", |v| v["restart"] == "Off")
        .await;
    let resp = b.call("plan-restart", &[]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    arm(&s, &mut b).await;
    eventually("restart accepted", || {
        count(&s, EventKind::RestartAccepted) == 1
    })
    .await;
    assert_eq!(updates(&s), 1);
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 1);
    assert_eq!(sleeps(&s), 0);
    let kinds = own_kinds(&s.state);
    let updated = kinds
        .iter()
        .position(|k| *k == EventKind::PcServiceUpdated)
        .expect("new version in the history");
    let restarted = kinds
        .iter()
        .position(|k| *k == EventKind::RestartAccepted)
        .unwrap();
    assert!(updated < restarted, "{kinds:?}");
}

#[tokio::test]
async fn arming_is_refused_without_an_update() {
    let (s, mut b, mut live) = ready("nothing-to-do", |_| {}).await;
    *s.pc.version.lock().unwrap() = Some(NEW.into());
    live.until("up to date", |v| v["pc_service"]["version"] == NEW)
        .await;
    let resp = b.call("plan-pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    let text = resp.text().await.unwrap();
    assert!(text.contains("already runs"), "{text}");
    assert_eq!(count(&s, EventKind::PcServiceUpdatePlanned), 0);
}

#[tokio::test]
async fn arming_is_refused_while_an_update_runs() {
    let (s, mut b, mut live) = ready("running", |_| {}).await;
    // The new version never reports: the update stays running.
    *s.pc.update_to.lock().unwrap() = None;
    s.state.pc_setup.set_update_timeout(Duration::from_secs(60));
    assert_eq!(b.call("pc-service-update", &[]).await.status(), 200);
    live.until("update running", |v| v["pc_service"]["updating"] == true)
        .await;
    let resp = b.call("plan-pc-service-update", &[]).await;
    assert_eq!(resp.status(), 400);
    let text = resp.text().await.unwrap();
    assert!(text.contains("running already"), "{text}");
    assert_eq!(count(&s, EventKind::PcServiceUpdatePlanned), 0);
}
