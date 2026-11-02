//! The Planned restart: *Restart at next sleep* turns the proxy's next
//! sleep request after the idle timeout into `POST /restart`.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::events::EventKind;

/// The idle timeout once a test lets it run out; until then an hour, so a
/// slow login under load never runs into it.
const IDLE_SECS: &str = "1";

/// A PC service that takes the Planned restart, its PC awake, with the
/// overview open and showing the button.
async fn ready(tweak: impl FnOnce(&mut strata_nas_proxy::Config)) -> (Setup, Browser, Live) {
    let s = setup(true, tweak).await;
    s.pc.takes_restart.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("restart offered", |v| {
        v["pc"] == "Awake" && v["restart"] == "Off"
    })
    .await;
    (s, b, live)
}

/// Lets the idle timeout run out `secs` from now.
fn go_idle(s: &Setup, secs: &str) {
    s.state.settings.set("IDLE_TIMEOUT_SECS", secs).unwrap();
    s.state.touch();
}

/// Arms the Planned restart.
async fn plan(b: &mut Browser) {
    let resp = b.call("plan-restart", &[]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

/// Arms the Planned restart and lets the idle timeout run out.
async fn arm(s: &Setup, b: &mut Browser) {
    plan(b).await;
    go_idle(s, IDLE_SECS);
}

fn planned(v: &Value) -> bool {
    v["restart"]["Planned"].is_object()
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

async fn accepted(s: &Setup) {
    eventually("restart accepted", || {
        count(s, EventKind::RestartAccepted) == 1
    })
    .await;
}

#[tokio::test]
async fn idle_timeout_restarts_a_pc_with_a_planned_restart() {
    let (s, mut b, _live) = ready(|_| {}).await;
    arm(&s, &mut b).await;
    accepted(&s).await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    let kinds = own_kinds(&s.state);
    assert!(
        kinds.ends_with(&[
            EventKind::RestartPlanned,
            EventKind::RestartAccepted,
            EventKind::SleepAccepted
        ]),
        "{kinds:?}"
    );
    assert_eq!(detail(&s, EventKind::RestartPlanned), "from the web UI");
}

#[tokio::test]
async fn idle_timeout_sleeps_a_pc_without_a_planned_restart() {
    let (s, _b, _live) = ready(|_| {}).await;
    go_idle(&s, IDLE_SECS);
    eventually("sleep request", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_accepted_restart_clears_the_plan() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    arm(&s, &mut b).await;
    accepted(&s).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    live.until("back", |v| v["pc"] == "Awake" && v["restart"] == "Off")
        .await;
    eventually("sleep request after the next idle timeout", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_restart_keeps_the_plan_and_shows_the_reason() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    let v = live
        .until("refusal shown", |v| {
            v["restart"]["Planned"]["refused"].is_string()
        })
        .await;
    assert_eq!(v["restart"]["Planned"]["refused"], "User active");
    assert_eq!(detail(&s, EventKind::RestartRefused), "User active");
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refused_restart_is_tried_again_after_the_sleep_retry() {
    let (s, mut b, _live) = ready(|c| c.sleep_retry = Duration::from_millis(300)).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    eventually("restart refused", || {
        count(&s, EventKind::RestartRefused) >= 1
    })
    .await;
    s.pc.refuse_sleep.store(false, Ordering::SeqCst);
    accepted(&s).await;
    assert!(s.pc.restart_requests.load(Ordering::SeqCst) >= 2);
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancel_ends_a_planned_restart() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    let resp = b.call("cancel-restart", &[]).await;
    assert_eq!(resp.status(), 200);
    live.until("off", |v| v["restart"] == "Off").await;
    assert_eq!(detail(&s, EventKind::RestartCancelled), "from the web UI");
    s.state.idle_tick().await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_planned_restart_survives_a_proxy_restart() {
    let dir = data_dir("planned-restart");
    let (s, mut b, mut live) = ready(|c| c.data_dir = Some(dir.clone())).await;
    plan(&mut b).await;
    live.until("planned", planned).await;
    drop(live);

    let again = restarted(&s);
    assert!(
        matches!(
            again.restart_view(),
            strata_admin_ui::api::RestartView::Planned { .. }
        ),
        "{:?}",
        again.restart_view()
    );
    // The idle timeout of the restarted proxy runs out at once.
    again.settings.set("IDLE_TIMEOUT_SECS", "0").unwrap();
    again.idle_tick().await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_planned_restart_can_be_armed_while_the_pc_sleeps() {
    let s = setup(false, |_| {}).await;
    s.pc.takes_restart.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("PC asleep", |v| v["pc"] == "Asleep").await;
    let resp = b.call("plan-restart", &[]).await;
    assert_eq!(resp.status(), 200);
    live.until("planned", planned).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn force_sleep_leaves_a_planned_restart_armed() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    plan(&mut b).await;
    let resp = b.call("force-sleep", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.force_sleeps.load(Ordering::SeqCst), 1);
    live.until("asleep and planned", |v| v["pc"] == "Asleep" && planned(v))
        .await;
}

#[tokio::test]
async fn sleep_now_leaves_a_planned_restart_armed() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    plan(&mut b).await;
    let resp = b.call("sleep-now", &[]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 0);
    live.until("asleep and planned", |v| v["pc"] == "Asleep" && planned(v))
        .await;
}

#[tokio::test]
async fn the_overview_says_restarting_after_an_accepted_restart() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    arm(&s, &mut b).await;
    live.until("restarting", |v| v["restart"] == "Restarting")
        .await;
    s.pc.awake.store(true, Ordering::SeqCst);
    live.until("back", |v| v["restart"] == "Off").await;
}

#[tokio::test]
async fn the_pc_back_after_a_restart_is_seen_awake_and_the_idle_timeout_starts_again() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    arm(&s, &mut b).await;
    accepted(&s).await;
    live.until("asleep", |v| v["pc"] == "Asleep").await;
    // Longer than the restart takes here: only the PC coming back can start
    // it again.
    s.state.settings.set("IDLE_TIMEOUT_SECS", "2").unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    s.pc.awake.store(true, Ordering::SeqCst);
    eventually("PC seen awake", || {
        kinds(&s.state).ends_with(&[EventKind::PcSeenAwake])
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    eventually("sleep request after a full idle timeout", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn a_request_while_the_pc_restarts_waits_for_it() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    arm(&s, &mut b).await;
    accepted(&s).await;
    live.until("asleep", |v| v["pc"] == "Asleep").await;
    let resp = post_json(&s, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn a_pc_not_back_after_the_restart_is_noted_once() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    s.state
        .set_restart_overdue_after(Duration::from_millis(500));
    arm(&s, &mut b).await;
    live.until("overdue", |v| v["restart"] == "Overdue").await;
    eventually("noted", || count(&s, EventKind::RestartOverdue) == 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&s, EventKind::RestartOverdue), 1);
}

#[tokio::test]
async fn the_not_back_note_ends_when_the_pc_service_answers() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    s.state
        .set_restart_overdue_after(Duration::from_millis(300));
    arm(&s, &mut b).await;
    live.until("overdue", |v| v["restart"] == "Overdue").await;
    s.pc.awake.store(true, Ordering::SeqCst);
    live.until("back", |v| v["restart"] == "Off").await;
}

#[tokio::test]
async fn the_button_needs_a_pc_service_that_takes_restart() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let mut live = Live::open(&b).await;
    live.until("unavailable", |v| {
        v["pc"] == "Awake" && v["restart"] == "Unavailable"
    })
    .await;
    let resp = b.call("plan-restart", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("Needs PC service 1.4.0")
    );
    assert_eq!(count(&s, EventKind::RestartPlanned), 0);
}

#[tokio::test]
async fn an_older_pc_service_gets_a_sleep_request_and_the_plan_is_cancelled() {
    let (s, mut b, mut live) = ready(|_| {}).await;
    s.pc.no_restart.store(true, Ordering::SeqCst);
    arm(&s, &mut b).await;
    eventually("sleep request", || {
        s.pc.sleep_requests.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        detail(&s, EventKind::RestartCancelled),
        "the PC service is too old for a Planned restart"
    );
    live.until("off", |v| v["restart"] == "Off").await;
}

#[tokio::test]
async fn keep_awake_holds_a_planned_restart() {
    let (s, mut b, _live) = ready(|_| {}).await;
    let resp = b.call("keep-awake", &[("choice", "1h")]).await;
    assert_eq!(resp.status(), 200);
    arm(&s, &mut b).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(s.pc.restart_requests.load(Ordering::SeqCst), 0);
}
