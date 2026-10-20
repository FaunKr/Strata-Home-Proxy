//! What a Strata update brings, on the Version tab (#141): the incoming
//! commits the PC service reports and the summary Strata writes of them.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};

/// Two incoming commits on a github.com remote, the installed version 477ac13.
fn with_update(remote: &str) -> StrataReport {
    let mut report = strata_info(true);
    report.info.update_available = Some(true);
    let commit = |hash: &str, time, subject: &str, body: &str| IncomingCommit {
        hash: hash.into(),
        time,
        subject: subject.into(),
        body: body.into(),
    };
    let file = |path: &str, added, removed| ChangedFile {
        path: path.into(),
        added,
        removed,
    };
    report.update = Some(Incoming {
        upstream: "fedcba9876543210".into(),
        total: 2,
        commits: vec![
            commit(
                "fedcba9876543210",
                1_760_000_000,
                "Faster prefill",
                "Reads the prompt in bigger chunks.\nRe-run setup afterwards.",
            ),
            commit("aaaabbbbccccdddd", 1_759_990_000, "Fix a crash", ""),
        ],
        files: vec![
            file("serve/server.py", 120, 30),
            file("serve/frontend.py", 10, 2),
            file("setup.py", 4, 0),
            file("docs/logo.png", 0, 0),
        ],
        remote: Some(remote.into()),
    });
    report
}

/// The incoming commits the fake PC service reports.
fn incoming(s: &Setup, change: impl FnOnce(&mut Incoming)) {
    let mut report = s.pc.strata_info.lock().unwrap();
    change(report.as_mut().unwrap().update.as_mut().unwrap());
}

async fn page(b: &mut Browser) -> Value {
    let resp = b.call("strata", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn the_version_tab_lists_the_incoming_commits() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_update("https://github.com/Niko1221/Strata.git"));
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    let update = &v["update"];
    let commits = update["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2, "{v}");
    assert_eq!(commits[0]["short"], "fedcba9");
    assert_eq!(commits[0]["subject"], "Faster prefill");
    assert!(!commits[0]["date_text"].as_str().unwrap().is_empty());
    assert_eq!(
        commits[0]["url"],
        "https://github.com/Niko1221/Strata/commit/fedcba9876543210"
    );
    assert_eq!(update["left_out"], 0);
    assert_eq!(
        update["compare_url"],
        "https://github.com/Niko1221/Strata/compare/477ac13...fedcba9876543210"
    );
    // The remote itself and the commit bodies stay on the NAS.
    let text = v.to_string();
    assert!(!text.contains("Strata.git"), "{v}");
    assert!(!text.contains("bigger chunks"), "{v}");
}

#[tokio::test]
async fn without_a_summary_the_card_says_how_much_changed_and_where() {
    let s = setup(true, |_| {}).await;
    let mut report = with_update("https://github.com/Niko1221/Strata.git");
    report.update.as_mut().unwrap().total = 230;
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    let update = &v["update"];
    assert_eq!(update["left_out"], 228, "{v}");
    assert_eq!(
        update["summary"]["fallback"],
        json!([
            "230 new commits since the installed version.",
            "Changes in serve/, setup.py, docs/."
        ]),
        "{v}"
    );
}

#[tokio::test]
async fn only_a_github_remote_gets_links() {
    for remote in [
        "git@github.com:Niko1221/Strata.git",
        "https://github.com/Niko1221/Strata",
    ] {
        let s = setup(true, |_| {}).await;
        *s.pc.strata_info.lock().unwrap() = Some(with_update(remote));
        let mut b = logged_in(&s.url).await;
        let v = page(&mut b).await;
        assert_eq!(
            v["update"]["commits"][1]["url"],
            "https://github.com/Niko1221/Strata/commit/aaaabbbbccccdddd",
            "{remote}"
        );
    }
    for remote in [
        "https://example.org/Strata.git",
        "https://github.com.evil.org/a/b.git",
    ] {
        let s = setup(true, |_| {}).await;
        *s.pc.strata_info.lock().unwrap() = Some(with_update(remote));
        let mut b = logged_in(&s.url).await;
        let v = page(&mut b).await;
        assert_eq!(v["update"]["commits"][0]["url"], Value::Null, "{remote}");
        assert_eq!(v["update"]["compare_url"], Value::Null, "{remote}");
    }
}

#[tokio::test]
async fn an_older_pc_service_shows_just_the_badge() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.update_available = Some(true);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["info"]["update_available"], true);
    assert_eq!(v["update"], Value::Null, "{v}");
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 0);
}

// --- #143 Strata writes the summary ---

const BULLETS: &str = "- **Re-run setup** after updating.\n- Prompts are read faster.\n* A crash on long answers is fixed.\nThat is all.";

/// An awake PC with an idle, loaded Strata and an update to summarize.
async fn ready() -> Setup {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_update("https://github.com/Niko1221/Strata.git"));
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    *s.pc.chat_answer.lock().unwrap() = Some(BULLETS.into());
    s
}

/// The page once the summary is no longer being written.
async fn settled(b: &mut Browser) -> Value {
    for _ in 0..100 {
        let v = page(b).await;
        if v["update"]["summary"]["state"] != "writing" {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the summary is still being written");
}

#[tokio::test]
async fn strata_writes_the_summary_once() {
    let s = ready().await;
    let mut b = logged_in(&s.url).await;
    let first = page(&mut b).await;
    assert_eq!(first["update"]["summary"]["state"], "writing", "{first}");
    assert_eq!(
        first["update"]["summary"]["fallback"][0],
        "2 new commits since the installed version."
    );
    let v = settled(&mut b).await;
    assert_eq!(
        v["update"]["summary"],
        json!({"state": "written", "model": "Qwen3-30B-A3B", "lines": [
            "Re-run setup after updating.",
            "Prompts are read faster.",
            "A crash on long answers is fixed.",
        ]}),
        "{v}"
    );
    page(&mut b).await;
    let bodies = s.pc.chat_bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "asked once");
    let body = &bodies[0];
    assert_eq!(body["reasoning_effort"], "medium");
    assert_ne!(body["stream"], true);
    let prompt = body["messages"].to_string();
    for part in [
        "Faster prefill",
        "bigger chunks",
        "Fix a crash",
        "serve/server.py",
    ] {
        assert!(prompt.contains(part), "{part}: {prompt}");
    }
}

#[tokio::test]
async fn new_upstream_commits_get_a_new_summary() {
    let s = ready().await;
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    settled(&mut b).await;
    incoming(&s, |update| {
        update.upstream = "1111222233334444".into();
        update.total = 3;
    });
    *s.pc.chat_answer.lock().unwrap() = Some("- Three changes now.".into());
    let v = page(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "writing", "{v}");
    let v = settled(&mut b).await;
    assert_eq!(
        v["update"]["summary"]["lines"],
        json!(["Three changes now."]),
        "{v}"
    );
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_busy_or_unready_strata_is_not_asked() {
    let unloaded = ready().await;
    unloaded.pc.unloaded.store(true, Ordering::SeqCst);
    let busy = ready().await;
    *busy.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    let job = ready().await;
    *job.pc.strata_job.lock().unwrap() = Some(StrataJob {
        id: Some(1),
        ..common::job(StrataJobKind::Benchmark, StrataJobState::Running)
    });
    for (what, s) in [("unloaded", unloaded), ("busy", busy), ("job", job)] {
        let mut b = logged_in(&s.url).await;
        page(&mut b).await;
        let v = settled(&mut b).await;
        assert_eq!(
            v["update"]["summary"]["state"], "unavailable",
            "{what}: {v}"
        );
        assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 0, "{what}");
    }
}

#[tokio::test]
async fn gaming_mode_does_not_ask_strata() {
    let s = ready().await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "unavailable", "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn a_failed_summary_falls_back_and_is_not_asked_again_right_away() {
    for how in ["error", "no bullets"] {
        let s = ready().await;
        match how {
            "error" => s.pc.chat_fails.store(true, Ordering::SeqCst),
            _ => *s.pc.chat_answer.lock().unwrap() = Some("I cannot tell.".into()),
        }
        let mut b = logged_in(&s.url).await;
        page(&mut b).await;
        let v = settled(&mut b).await;
        assert_eq!(v["update"]["summary"]["state"], "unavailable", "{how}: {v}");
        page(&mut b).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 1, "{how}");
    }
}

#[tokio::test]
async fn the_summary_is_housekeeping_not_use() {
    let s = ready().await;
    let before = s.state.snapshot().last_activity_at;
    let mut b = logged_in(&s.url).await;
    let events = own_kinds(&s.state);
    page(&mut b).await;
    let v = settled(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "written", "{v}");
    assert_eq!(s.state.snapshot().last_activity_at, before);
    assert_eq!(own_kinds(&s.state), events);
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

// --- #144 kept on the NAS ---

fn data_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("strata-update-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A proxy with a `DATA_DIR` whose summary Strata has written.
async fn written(dir: &std::path::Path) -> (Setup, Browser) {
    let data = dir.to_path_buf();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_update("https://github.com/Niko1221/Strata.git"));
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    *s.pc.chat_answer.lock().unwrap() = Some(BULLETS.into());
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    assert_eq!(
        settled(&mut b).await["update"]["summary"]["state"],
        "written"
    );
    (s, b)
}

fn restarted(s: &Setup) -> std::sync::Arc<strata_nas_proxy::AppState> {
    strata_nas_proxy::AppState::new((*s.state.cfg()).clone())
}

#[tokio::test]
async fn the_summary_survives_a_restart_of_the_proxy() {
    let dir = data_dir("restart");
    let (s, _) = written(&dir).await;
    let state = restarted(&s);
    let p = strata_nas_proxy::strata::page(&state).await;
    let update = p.update.expect("an update");
    assert!(
        matches!(&update.summary, strata_admin_ui::api::UpdateSummary::Written { lines, .. } if lines.len() == 3),
        "{update:?}"
    );
    assert_eq!(update.commits.len(), 2);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 1, "not asked again");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_sleeping_pc_still_shows_the_summary() {
    let dir = data_dir("asleep");
    let (s, mut b) = written(&dir).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let v = page(&mut b).await;
    assert_eq!(v["stale"], true, "{v}");
    assert_eq!(v["update"]["summary"]["state"], "written", "{v}");
    assert_eq!(v["update"]["commits"].as_array().unwrap().len(), 2, "{v}");

    // After a restart the commits come back with the PC; the summary stays.
    let state = restarted(&s);
    let p = strata_nas_proxy::strata::page(&state).await;
    let update = p.update.expect("an update");
    assert!(update.commits.is_empty(), "{update:?}");
    assert!(
        matches!(
            update.summary,
            strata_admin_ui::api::UpdateSummary::Written { .. }
        ),
        "{update:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_summary_goes_once_the_update_is_installed() {
    let dir = data_dir("installed");
    let (s, mut b) = written(&dir).await;
    {
        let mut report = s.pc.strata_info.lock().unwrap();
        let report = report.as_mut().unwrap();
        report.info.version = Some("fedcba9".into());
        report.info.update_available = Some(false);
        report.update = None;
    }
    let v = page(&mut b).await;
    assert_eq!(v["update"], Value::Null, "{v}");
    s.pc.awake.store(false, Ordering::SeqCst);
    let state = restarted(&s);
    assert_eq!(strata_nas_proxy::strata::page(&state).await.update, None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_summary_of_older_commits_is_not_shown_for_newer_ones() {
    let dir = data_dir("outdated");
    let (s, mut b) = written(&dir).await;
    incoming(&s, |update| update.upstream = "1111222233334444".into());
    s.pc.chat_fails.store(true, Ordering::SeqCst);
    page(&mut b).await;
    assert_eq!(
        settled(&mut b).await["update"]["summary"]["state"],
        "unavailable"
    );
    s.pc.awake.store(false, Ordering::SeqCst);
    let v = page(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "unavailable", "{v}");
    let state = restarted(&s);
    assert_eq!(strata_nas_proxy::strata::page(&state).await.update, None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_slow_strata_hits_the_limit_and_is_asked_again_later() {
    let s = ready().await;
    s.state.strata.update.set_timing(
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(2500),
    );
    *s.pc.chat_pause.lock().unwrap() = std::time::Duration::from_millis(1000);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let v = settled(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "unavailable", "{v}");
    // Not again right away …
    page(&mut b).await;
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 1);
    // … but once the wait is over, and now Strata answers in time.
    *s.pc.chat_pause.lock().unwrap() = std::time::Duration::ZERO;
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
    page(&mut b).await;
    let v = settled(&mut b).await;
    assert_eq!(v["update"]["summary"]["state"], "written", "{v}");
    assert_eq!(s.pc.chat_bodies.lock().unwrap().len(), 2);
}
