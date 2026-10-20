//! The Strata process on its own (#184): restart, the wait for an idle
//! Strata, rollback after a config change and the loading window, on the
//! fake platform without a job request over HTTP.

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::*;
use serde_json::json;
use strata_pc_agent::{
    job::{Step, testing},
    strata_process::Applied,
};

#[tokio::test]
async fn a_restart_stops_and_starts_strata_unless_gaming_mode_is_on() {
    let s = setup("process-restart").await;
    s.agent.strata().restart().await;
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);

    s.agent.set_gaming(true);
    s.agent.strata().restart().await;
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 3, "gaming on, restart");
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        1,
        "no start in gaming mode"
    );
}

#[tokio::test]
async fn a_restart_waits_the_settle_pause_between_stop_and_start() {
    let s = setup_with("process-settle", |c| c.stop_settle_ms = 300).await;
    let agent = s.agent.clone();
    let restart = tokio::spawn(async move { agent.strata().restart().await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0, "still settling");
    restart.await.unwrap();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_restart_when_idle_waits_for_the_running_request() {
    let s = setup("process-idle").await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let job = testing::job(&s.agent);
    let agent = s.agent.clone();
    let restart = tokio::spawn(async move { agent.strata().restart_when_idle(&job).await });
    job_until(&s, "Warten auf die Anfrage", |j| {
        j["step"] == "Waiting until Strata finishes the running request"
    })
    .await;
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        0,
        "stopped mid-request"
    );
    s.fake.strata_busy.store(false, Ordering::SeqCst);
    restart.await.unwrap().unwrap();
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(get(&s, "/strata/job").await["step"], "Restarting Strata");
}

#[tokio::test]
async fn a_restart_when_idle_ends_on_cancel_without_a_restart() {
    let s = setup("process-idle-cancel").await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let job = testing::job(&s.agent);
    let agent = s.agent.clone();
    let restart = tokio::spawn(async move { agent.strata().restart_when_idle(&job).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    assert!(matches!(restart.await.unwrap(), Err(Step::Cancelled)));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

/// A restore that notes it ran.
fn restore(ran: &Arc<AtomicBool>) -> impl FnOnce() -> std::future::Ready<Result<(), Step>> + use<> {
    let ran = Arc::clone(ran);
    move || {
        ran.store(true, Ordering::SeqCst);
        std::future::ready(Ok(()))
    }
}

#[tokio::test]
async fn a_change_strata_comes_up_with_is_kept() {
    let s = setup("process-kept").await;
    let job = testing::job(&s.agent);
    let ran = Arc::new(AtomicBool::new(false));
    let applied = s
        .agent
        .strata()
        .restart_or_roll_back(&job, "Restoring", restore(&ran))
        .await
        .unwrap();
    assert_eq!(applied, Applied::Kept);
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_change_strata_does_not_come_up_with_is_rolled_back() {
    let s = setup("process-rolled-back").await;
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    // Strata prints its error and keeps running without answering.
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: vec!["[X] out of memory".into()],
            hang: true,
            ..FakeCommand::default()
        },
    );
    let job = testing::job(&s.agent);
    let ran = Arc::new(AtomicBool::new(false));
    let applied = s
        .agent
        .strata()
        .restart_or_roll_back(&job, "Restoring", restore(&ran))
        .await
        .unwrap();
    assert_eq!(applied, Applied::RolledBack);
    assert!(ran.load(Ordering::SeqCst));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 2);
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        2,
        "again with the old config"
    );
    let shown = get(&s, "/strata/job").await;
    assert_eq!(shown["step"], "Restoring");
    assert_eq!(shown["lines"], json!(["[X] out of memory"]), "{shown}");
}

#[tokio::test]
async fn a_cancel_while_strata_comes_up_rolls_back_and_ends_cancelled() {
    let s = setup_with("process-rollback-cancel", |c| c.context_start_secs = 60).await;
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    let job = testing::job(&s.agent);
    let ran = Arc::new(AtomicBool::new(false));
    let agent = s.agent.clone();
    let restore = restore(&ran);
    let applied = tokio::spawn(async move {
        agent
            .strata()
            .restart_or_roll_back(&job, "Restoring", restore)
            .await
    });
    job_until(&s, "Warten auf Strata", |j| {
        j["step"] == "Waiting until Strata is ready"
    })
    .await;
    post(&s, "/strata/job/cancel", json!({})).await;
    assert!(matches!(applied.await.unwrap(), Err(Step::Cancelled)));
    assert!(ran.load(Ordering::SeqCst), "the old config comes back");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_cancel_while_strata_is_busy_puts_the_old_config_back_without_a_restart() {
    let s = setup("process-rollback-busy").await;
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let job = testing::job(&s.agent);
    let ran = Arc::new(AtomicBool::new(false));
    let agent = s.agent.clone();
    let restore = restore(&ran);
    let applied = tokio::spawn(async move {
        agent
            .strata()
            .restart_or_roll_back(&job, "Restoring", restore)
            .await
    });
    job_until(&s, "Warten auf die Anfrage", |j| {
        j["step"] == "Waiting until Strata finishes the running request"
    })
    .await;
    post(&s, "/strata/job/cancel", json!({})).await;
    assert!(matches!(applied.await.unwrap(), Err(Step::Cancelled)));
    assert!(ran.load(Ordering::SeqCst), "the old config comes back");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failed_restore_still_shows_strata_s_last_lines() {
    let s = setup("process-restore-fails").await;
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: vec!["[X] out of memory".into()],
            ..FakeCommand::default()
        },
    );
    let job = testing::job(&s.agent);
    let result = s
        .agent
        .strata()
        .restart_or_roll_back(&job, "Restoring", || {
            std::future::ready(Err(Step::Failed("config locked".into())))
        })
        .await;
    assert!(matches!(result, Err(Step::Failed(e)) if e == "config locked"));
    let shown = get(&s, "/strata/job").await;
    assert_eq!(shown["lines"], json!(["[X] out of memory"]), "{shown}");
}

#[tokio::test]
async fn strata_counts_as_loading_for_the_load_time_after_a_start() {
    let s = setup("process-loading").await;
    assert!(!s.agent.strata().loading(), "never started");
    s.agent.strata().start();
    assert!(s.agent.strata().loading());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!s.agent.strata().loading(), "load_time_secs = 1 is over");
}
