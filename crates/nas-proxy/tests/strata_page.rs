//! The Strata page: what the PC service reports about Strata, and jobs
//! started from the page, against the simulated PC and its service.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_admin_ui::labels::{STRATA_CONNECTION, STRATA_TIMEOUTS};
use strata_nas_proxy::events::EventKind;

async fn page(b: &mut Browser) -> Value {
    let resp = b.call("strata", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

async fn install(b: &mut Browser) -> reqwest::Response {
    b.call(
        "strata-job",
        &[
            ("kind", "install"),
            ("family", "qwen"),
            ("size", "IQ3_XXS"),
            ("context", "65536"),
        ],
    )
    .await
}

fn last_body(s: &Setup) -> Value {
    s.pc.job_bodies.lock().unwrap().last().cloned().unwrap()
}

#[tokio::test]
async fn needs_a_login() {
    let s = setup(false, |_| {}).await;
    let mut b = Browser::new(&s.url);
    for (name, fields) in [
        ("strata", vec![]),
        ("strata-job", vec![("kind", "update")]),
        ("strata-cancel", vec![]),
    ] {
        assert_eq!(b.call(name, &fields).await.status(), 401, "{name}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert!(s.pc.job_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shows_what_the_pc_service_reports() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["problem"], Value::Null);
    assert_eq!(v["stale"], false);
    assert_eq!(v["info"]["installed"], true);
    assert_eq!(v["info"]["models"][0]["id"], "IQ3_XXS");
    assert_eq!(v["info"]["recommended"], "IQ3_XXS");
}

#[tokio::test]
async fn a_sleeping_pc_shows_the_last_answer_and_stays_asleep() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let v = page(&mut b).await;
    assert!(v["problem"].as_str().unwrap().contains("asleep"), "{v}");
    assert_eq!(v["stale"], true);
    assert_eq!(v["info"]["installed"], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_older_pc_service_asks_for_an_update() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert!(
        v["problem"]
            .as_str()
            .unwrap()
            .contains("update the PC service"),
        "{v}"
    );
}

#[tokio::test]
async fn install_wakes_the_pc_and_sends_no_strata_key() {
    let s = setup(false, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(false));
    let mut b = logged_in(&s.url).await;
    let resp = install(&mut b).await;
    let text = resp.text().await.unwrap();
    assert!(s.pc.up(), "{text}");

    let body = last_body(&s);
    assert_eq!(body["kind"], "install");
    assert_eq!(body["family"], "qwen");
    assert_eq!(body["size"], "IQ3_XXS");
    assert_eq!(body["context"], 65536);
    // The PC service makes the key; an older one wants a valid one here.
    assert_eq!(body["api_key"].as_str().unwrap().len(), 64);
    let kinds = own_kinds(&s.state);
    assert_eq!(
        kinds.last(),
        Some(&EventKind::StrataJobStarted),
        "{kinds:?}"
    );
    assert!(kinds.contains(&EventKind::WakeRequested), "{kinds:?}");
}

#[tokio::test]
async fn a_status_from_before_the_start_does_not_end_the_job() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    b.call("strata-job", &[("kind", "benchmark")]).await;
    // Like a status answered just before the job began.
    let stale = PcStatus::default();
    strata_nas_proxy::strata::note_status(&s.state, &s.state.agent(), &stale).await;
    assert!(!kinds(&s.state).contains(&EventKind::StrataJobDone));
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually("job done", || {
        kinds(&s.state).contains(&EventKind::StrataJobDone)
    })
    .await;
}

#[tokio::test]
async fn the_page_offers_no_new_strata_key() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b.call("strata-job", &[("kind", "set_key")]).await;
    assert_eq!(resp.status(), 400);
    assert!(s.pc.job_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refusal_comes_with_its_reason() {
    let s = setup(true, |_| {}).await;
    *s.pc.refuse_job.lock().unwrap() = Some("Not enough space on C:".into());
    let mut b = logged_in(&s.url).await;
    let resp = install(&mut b).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("Not enough space on C:")
    );
    assert!(!kinds(&s.state).contains(&EventKind::StrataJobStarted));
}

#[tokio::test]
async fn only_fields_of_the_job_reach_the_pc_service() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[
                ("kind", "activate"),
                ("model", "coder-IQ1_M"),
                ("size", "x"),
                ("speed_projection", "true"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        last_body(&s),
        json!({"kind": "activate", "model": "coder-IQ1_M"})
    );
}

#[tokio::test]
async fn progress_comes_through_the_live_stream() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    let mut live = Live::open(&b).await;
    live.until("job", |v| v["strata_job"]["kind"] == "install")
        .await;
    s.pc.update_job(|j| {
        j.step = Some("4/7: loading model".into());
        j.percent = Some(40);
    });
    live.until("40 %", |v| v["strata_job"]["percent"] == 40)
        .await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    live.until("end", |v| v["strata_job"].is_null()).await;
    let kinds = own_kinds(&s.state);
    assert!(kinds.contains(&EventKind::StrataJobDone), "{kinds:?}");
}

#[tokio::test]
async fn without_a_page_nobody_asks_for_progress() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    // At most one look at the job: a status asked for just before the job
    // started says "no job", and the job itself tells whether it ended.
    let polls = s.pc.job_polls.load(Ordering::SeqCst);
    assert!(polls <= 1, "{polls}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.job_polls.load(Ordering::SeqCst), polls);
}

#[tokio::test]
async fn a_failed_job_is_recorded_with_why() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    s.pc.update_job(|j| {
        j.state = StrataJobState::Failed;
        j.error = Some("[X] Download aborted".into());
    });
    eventually("failure recorded", || {
        kinds(&s.state).contains(&EventKind::StrataJobFailed)
    })
    .await;
    let event = s.state.events.recent().pop().unwrap();
    assert!(
        event.detail.unwrap().contains("[X] Download aborted"),
        "reason missing"
    );
}

#[tokio::test]
async fn cancel_ends_the_job() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    assert_eq!(b.call("strata-cancel", &[]).await.status(), 200);
    assert_eq!(s.pc.job_cancels.load(Ordering::SeqCst), 1);
    eventually("cancellation recorded", || {
        kinds(&s.state).contains(&EventKind::StrataJobCancelled)
    })
    .await;
    let resp = b.call("strata-cancel", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("No job is running"));
}

#[tokio::test]
async fn the_page_offers_the_setup_with_the_recommendation() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(false));
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("Set up Strata"), "{html}");
    assert!(html.contains("(recommended)"), "{html}");
}

#[tokio::test]
async fn the_page_lists_installed_models_without_waking() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let html = b.page("/admin/strata").await;
    assert!(html.contains("Installed models"), "{html}");
    assert!(html.contains("IQ3_XXS"), "{html}");
    assert!(html.contains("81.0 GB"), "{html}");
    assert!(html.contains("asleep"), "{html}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn shows_the_last_benchmark_with_its_local_time() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.update_available = Some(true);
    report.info.benchmark = Some(benchmark("IQ3_XXS", "38.5 Token/s"));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["info"]["update_available"], true);
    assert_eq!(v["info"]["benchmark"]["result"], "38.5 Token/s");
    assert!(
        !v["info"]["benchmark"]["at_text"]
            .as_str()
            .unwrap()
            .is_empty(),
        "{v}"
    );
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("38.5 Token/s"), "{html}");
    assert!(html.contains("Update available"), "{html}");
}

#[tokio::test]
async fn shows_each_models_benchmark_fastest_first() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models = vec![
        model("IQ3_XXS", Some("qwen"), 1, true),
        model("coder-IQ1_M", Some("coder"), 1, false),
        model("swift-IQ2_XS", Some("swift"), 1, false),
    ];
    report.info.benchmarks = vec![
        StrataBenchmark {
            decode_tps: Some(38.5),
            prompt_tps: Some(612.0),
            context: Some(65536),
            speed_projection: Some(false),
            version: Some("477ac13".into()),
            ..benchmark("IQ3_XXS", "38.5 Token/s")
        },
        StrataBenchmark {
            decode_tps: Some(52.0),
            context: Some(32768),
            speed_projection: Some(true),
            ..benchmark("coder-IQ1_M", "52 Token/s")
        },
        StrataBenchmark {
            error: Some("Strata did not come up".into()),
            ..benchmark("swift-IQ2_XS", "")
        },
    ];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata/version").await;
    let coder = html.find("coder-IQ1_M").unwrap();
    let qwen = html.find("Decode 38.5 tok/s").unwrap();
    let swift = html.find("No result: Strata did not come up").unwrap();
    assert!(coder < qwen && qwen < swift, "{html}");
    assert!(html.contains("Decode 52 tok/s · Prompt –"), "{html}");
    assert!(html.contains("Prompt 612 tok/s"), "{html}");
    assert!(html.contains("32K context · speed projection on"), "{html}");
    assert!(html.contains("Strata 477ac13"), "{html}");
    assert!(html.contains("Fastest"), "{html}");
    // The fastest, inactive model can be made active; the failed one not.
    assert_eq!(html.matches("Make active").count(), 1, "{html}");
    assert!(html.contains("Compare models"), "{html}");
}

#[tokio::test]
async fn on_the_terminal_bench_bat_results_are_marked_not_comparable() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.install = Some(StrataInstall::Terminal);
    report.info.models = vec![
        model("IQ3_XXS", Some("qwen"), 1, true),
        model("coder-IQ1_M", Some("coder"), 1, false),
    ];
    report.info.benchmarks = vec![
        StrataBenchmark {
            decode_tps: Some(52.0),
            ..benchmark("coder-IQ1_M", "52 Token/s")
        },
        StrataBenchmark {
            decode_tps: Some(38.5),
            tool: Some(BenchTool::StrataBench),
            ..benchmark("IQ3_XXS", "38.5 Token/s")
        },
    ];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata/version").await;
    assert_eq!(
        html.matches("BENCH.bat, not comparable").count(),
        1,
        "{html}"
    );
    assert!(
        html.contains("Results marked BENCH.bat came from the old Strata folder"),
        "{html}"
    );
    // The faster old result is not the fastest.
    let fastest = html.find("Fastest").unwrap();
    assert!(
        html[..fastest].rfind("IQ3_XXS") > html[..fastest].rfind("coder-IQ1_M"),
        "{html}"
    );
}

#[tokio::test]
async fn a_strata_folder_from_source_shows_no_note() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.install = Some(StrataInstall::Source);
    report.info.benchmarks = vec![StrataBenchmark {
        decode_tps: Some(38.5),
        ..benchmark("IQ3_XXS", "38.5 Token/s")
    }];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata/version").await;
    assert!(!html.contains("not comparable"), "{html}");
}

#[tokio::test]
async fn comparing_models_reaches_the_pc_service() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b.call("strata-job", &[("kind", "compare_models")]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(last_body(&s), json!({"kind": "compare_models"}));
}

#[tokio::test]
async fn another_model_gets_no_strata_key_from_the_proxy() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("strata-secret-0123456789");
    s.state.ask_pc(&s.state.agent()).await.unwrap();
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[
                ("kind", "add_model"),
                ("family", "swift"),
                ("size", "IQ2_XS"),
                ("context", "65536"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(last_body(&s).get("api_key"), None);
}

#[tokio::test]
async fn each_tab_has_its_own_path() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let b = logged_in(&s.url).await;
    for (path, content) in [
        ("/admin/strata", "Load another model"),
        ("/admin/strata/output", "Console output"),
        ("/admin/strata/version", "Run benchmark"),
        (
            "/admin/strata/connection",
            "makes a new one at every start of Strata",
        ),
    ] {
        let html = b.page(path).await;
        assert!(html.contains(content), "{path}: {html}");
        assert!(html.contains("aria-current=\"page\""), "{path}: {html}");
    }
}

#[tokio::test]
async fn the_speed_projection_reaches_the_pc_service_when_ticked() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    assert_eq!(last_body(&s).get("experimental_speed_projection"), None);
    for kind in ["install", "add_model"] {
        let resp = b
            .call(
                "strata-job",
                &[
                    ("kind", kind),
                    ("family", "qwen"),
                    ("size", "IQ3_XXS"),
                    ("context", "65536"),
                    ("speed_projection", "true"),
                ],
            )
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            last_body(&s)["experimental_speed_projection"],
            true,
            "{kind}"
        );
    }
}

#[tokio::test]
async fn connection_tab_holds_exactly_the_strata_settings() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains("id=\"setting-STRATA_URL\""), "{html}");
    assert!(!html.contains("STRATA_API_KEY"), "{html}");
    assert!(html.contains("id=\"setting-CODE_BENCH_URL\""), "{html}");
    let fields = html.matches("id=\"setting-").count();
    let keys = STRATA_CONNECTION.keys.len() + STRATA_TIMEOUTS.keys.len();
    assert_eq!(fields, keys, "{html}");
    let models = b.page("/admin/strata").await;
    assert!(!models.contains("id=\"setting-"), "{models}");
}

#[tokio::test]
async fn a_running_job_shows_and_locks_the_buttons_in_every_tab() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report
        .info
        .models
        .push(model("IQ2_M", None, 60_000_000_000, false));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    *s.pc.strata_job.lock().unwrap() = Some(StrataJob {
        percent: Some(40),
        ..job(StrataJobKind::Update, StrataJobState::Running)
    });
    let b = logged_in(&s.url).await;
    for (path, button) in [
        ("/admin/strata", "Make active"),
        ("/admin/strata/version", "Run benchmark"),
    ] {
        let html = b.page(path).await;
        assert!(html.contains("40 %"), "{path}: {html}");
        let at = html
            .find(&format!(">{button}<"))
            .unwrap_or_else(|| panic!("{path}: {html}"));
        let tag = &html[html[..at].rfind("<button").unwrap()..at];
        assert!(tag.contains("disabled"), "{path}: {tag}");
    }
}

#[tokio::test]
async fn the_setup_offers_the_speed_projection_with_what_it_does() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(false));
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("Experimental speed projection"), "{html}");
    assert!(html.contains("refusal direction"), "{html}");
}

// --- #76 the speed projection of installed models ---

/// Strata with IQ3_XXS (active) and swift-IQ2_XS; `projection` is what the
/// PC service says about IQ3_XXS, `None` like an older one.
fn with_two_models(projection: Option<bool>) -> StrataReport {
    let mut report = strata_info(true);
    report.info.families = vec![
        family("qwen", "Qwen3.8-Flash-Next", true),
        family("swift", "Swift 1.5", false),
    ];
    let mut qwen = model("IQ3_XXS", Some("qwen"), 81_000_000_000, true);
    let mut swift = model("swift-IQ2_XS", Some("swift"), 40_000_000_000, false);
    if let Some(on) = projection {
        qwen.speed_projection = Some(on);
        swift.speed_projection = Some(false);
    }
    report.info.models = vec![qwen, swift];
    report
}

#[tokio::test]
async fn an_installed_model_shows_its_speed_projection() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(Some(true)));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(html.matches("Speed projection on").count(), 1, "{html}");
}

#[tokio::test]
async fn without_word_from_the_pc_service_no_speed_projection_shows() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("IQ3_XXS"), "{html}");
    assert!(!html.contains("Speed projection on"), "{html}");
}

#[tokio::test]
async fn switching_the_speed_projection_sends_only_its_fields() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[
                ("kind", "set_speed_projection"),
                ("model", "IQ3_XXS"),
                ("speed_projection", "false"),
                ("family", "qwen"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        last_body(&s),
        json!({"kind": "set_speed_projection", "model": "IQ3_XXS", "on": false})
    );
}

#[tokio::test]
async fn only_models_that_take_it_offer_to_switch_the_speed_projection() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(Some(true)));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(
        html.matches("Switch speed projection off").count(),
        1,
        "{html}"
    );
    assert!(!html.contains("Switch speed projection on"), "{html}");
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(Some(false)));
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(
        html.matches("Switch speed projection on").count(),
        1,
        "{html}"
    );
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(!html.contains("Switch speed projection"), "{html}");
}

// --- #101 the context of installed models ---

#[tokio::test]
async fn an_installed_model_shows_its_context() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].context = Some(65536);
    report.info.models[1].context = Some(524288);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("64K context"), "{html}");
    assert!(html.contains("512K context"), "{html}");
}

#[tokio::test]
async fn without_word_from_the_pc_service_no_context_shows() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("IQ3_XXS"), "{html}");
    assert!(!html.contains("K context"), "{html}");
}

// --- #102 switching the context of an installed model ---

#[tokio::test]
async fn switching_the_context_sends_only_its_fields() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call(
            "strata-job",
            &[
                ("kind", "set_context"),
                ("model", "swift-IQ2_XS"),
                ("context", "131072"),
                ("family", "swift"),
                ("speed_projection", "true"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        last_body(&s),
        json!({"kind": "set_context", "model": "swift-IQ2_XS", "context": 131072})
    );
}

#[tokio::test]
async fn a_model_with_a_known_context_offers_to_change_it() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.contexts = vec![8192, 65536, 262144, 524288];
    report.info.models[1].context = Some(524288);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(html.matches("Change context").count(), 1, "{html}");
    assert!(html.contains("256K tokens"), "{html}");
    assert!(
        !html.contains("512K tokens"),
        "past 256K only setup: {html}"
    );
}

#[tokio::test]
async fn without_a_known_context_there_is_nothing_to_change() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(!html.contains("Change context"), "{html}");
}

// --- #103 the context of the active model ---

#[tokio::test]
async fn the_active_model_offers_to_change_its_context_too() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].context = Some(65536);
    report.info.models[1].context = Some(65536);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(html.matches("Change context").count(), 2, "{html}");
}

// --- #158 loop recovery of installed models ---

#[tokio::test]
async fn an_installed_model_shows_its_loop_recovery() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].loop_recovery = Some("recover".into());
    report.info.models[1].loop_recovery = Some("off".into());
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("loop recovery on"), "{html}");
    assert!(html.contains("loop recovery off"), "{html}");
}

#[tokio::test]
async fn without_word_from_the_pc_service_no_loop_recovery_shows() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("IQ3_XXS"), "{html}");
    assert!(!html.contains("loop recovery"), "{html}");
}

// --- #159 the sampling of installed models ---

fn sampling_field(name: &str, value: f64, source: SamplingSource) -> SamplingField {
    SamplingField {
        name: name.into(),
        value,
        source,
    }
}

#[tokio::test]
async fn an_installed_model_shows_its_sampling_and_its_sources() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].sampling = Some(StrataSampling {
        support: SamplingSupport::ModelCard,
        thinking: vec![
            sampling_field("temperature", 1.0, SamplingSource::ModelCard),
            sampling_field("top_p", 0.95, SamplingSource::ModelCard),
            sampling_field("top_k", 20.0, SamplingSource::ModelCard),
        ],
        instruct: vec![
            sampling_field("temperature", 0.6, SamplingSource::Config),
            sampling_field("presence_penalty", 1.5, SamplingSource::ModelCard),
        ],
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(
        html.contains("With thinking: temperature 1 · top_p 0.95 · top_k 20"),
        "{html}"
    );
    assert!(
        html.contains("Without thinking: temperature 0.6 (config) · presence_penalty 1.5"),
        "{html}"
    );
    assert!(html.contains("from the model card"), "{html}");
    assert!(html.contains("own sampling fields still win"), "{html}");
}

#[tokio::test]
async fn a_strata_that_ignores_sampling_says_to_update_it() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].sampling = Some(StrataSampling {
        support: SamplingSupport::None,
        thinking: vec![],
        instruct: vec![],
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(
        html.contains("This Strata version ignores sampling; update it."),
        "{html}"
    );
}

#[tokio::test]
async fn without_word_from_the_pc_service_no_sampling_shows() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(with_two_models(None));
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(!html.contains("thinking:"), "{html}");
    assert!(!html.contains("still win"), "{html}");
}

// --- #160 Chat settings over the recommended sampling ---

#[tokio::test]
async fn sampling_from_chat_settings_is_marked_as_such() {
    let s = setup(true, |_| {}).await;
    let mut report = with_two_models(None);
    report.info.models[0].sampling = Some(StrataSampling {
        support: SamplingSupport::ModelCard,
        thinking: vec![
            sampling_field("temperature", 0.3, SamplingSource::ChatSettings),
            sampling_field("top_k", 20.0, SamplingSource::ModelCard),
        ],
        instruct: vec![],
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    page(&mut b).await;
    let html = b.page("/admin/strata").await;
    assert!(
        html.contains("With thinking: temperature 0.3 (Chat settings) · top_k 20"),
        "{html}"
    );
    assert!(html.contains("Without thinking: greedy"), "{html}");
}

// --- #208 the last answer kept over a restart ---

/// A proxy with `dir` as `DATA_DIR` whose PC service answered with `report`
/// and the running `job`.
async fn answered(dir: &std::path::Path, report: StrataReport, job: Option<StrataJob>) -> Setup {
    let data = dir.to_path_buf();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    *s.pc.strata_info.lock().unwrap() = Some(report);
    *s.pc.strata_job.lock().unwrap() = job;
    let mut b = logged_in(&s.url).await;
    assert_eq!(page(&mut b).await["stale"], false);
    s
}

/// The proxy of `s` started again, with the PC asleep.
async fn restarted_asleep(s: &Setup) -> Browser {
    s.pc.awake.store(false, Ordering::SeqCst);
    logged_in_again(&serve_proxy(restarted(s)).await).await
}

#[tokio::test]
async fn a_restarted_proxy_shows_the_last_answer_while_the_pc_sleeps() {
    let dir = data_dir("restart");
    let mut report = strata_info(true);
    report.info.update_available = Some(true);
    report.info.benchmarks = vec![benchmark("IQ3_XXS", "ok")];
    let s = answered(&dir, report, None).await;

    let mut b = restarted_asleep(&s).await;
    let v = page(&mut b).await;
    assert!(v["problem"].as_str().unwrap().contains("asleep"), "{v}");
    assert_eq!(v["stale"], true, "{v}");
    assert_eq!(v["info"]["version"], "477ac13", "{v}");
    assert_eq!(v["info"]["update_available"], true, "{v}");
    assert_eq!(v["info"]["models"][0]["id"], "IQ3_XXS", "{v}");
    assert_eq!(v["info"]["benchmarks"][0]["model"], "IQ3_XXS", "{v}");
    let as_of = v["as_of"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(as_of.contains(':'), "{v}");

    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("477ac13"), "{html}");
    assert!(html.contains("Update available"), "{html}");
    assert!(html.contains(as_of), "{html}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_answer_from_now_has_no_as_of() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    assert_eq!(page(&mut b).await["as_of"], Value::Null);
}

#[tokio::test]
async fn a_job_running_at_the_last_answer_is_not_kept() {
    let dir = data_dir("job");
    let s = answered(
        &dir,
        strata_info(true),
        Some(job(StrataJobKind::Benchmark, StrataJobState::Running)),
    )
    .await;

    let mut b = restarted_asleep(&s).await;
    let v = page(&mut b).await;
    assert_eq!(v["stale"], true, "{v}");
    assert_eq!(v["info"]["job"], Value::Null, "{v}");
    let html = b.page("/admin/strata/version").await;
    let at = html
        .find(">Update Strata<")
        .unwrap_or_else(|| panic!("{html}"));
    let tag = &html[html[..at].rfind("<button").unwrap()..at];
    assert!(!tag.contains("disabled"), "{tag}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_unreadable_last_answer_counts_as_none() {
    let dir = data_dir("broken");
    let s = answered(&dir, strata_info(true), None).await;
    std::fs::write(dir.join("strata-last.json"), "{ kaputt").unwrap();

    let mut b = restarted_asleep(&s).await;
    let v = page(&mut b).await;
    assert!(v["problem"].as_str().unwrap().contains("asleep"), "{v}");
    assert_eq!(v["stale"], false, "{v}");
    assert_eq!(v["info"], Value::Null, "{v}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn update_from_the_kept_answer_wakes_the_pc() {
    let dir = data_dir("update");
    let s = answered(&dir, strata_info(true), None).await;

    let mut b = restarted_asleep(&s).await;
    assert_eq!(page(&mut b).await["stale"], true);
    let resp = b.call("strata-job", &[("kind", "update")]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) > 0);
    assert_eq!(last_body(&s)["kind"], "update");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Strata as `install` with an old Strata folder, served or not.
fn on(install: StrataInstall, old_folder: bool, served: bool) -> StrataReport {
    let mut report = strata_info(true);
    report.info.install = Some(install);
    report.info.old_strata_folder = old_folder;
    report.info.terminal_served = served;
    report
}

#[tokio::test]
async fn the_switch_and_the_removal_show_by_the_pc_s_state() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    for (report, switch, remove) in [
        (on(StrataInstall::Source, true, false), true, false),
        // An older PC service knows no switch.
        (on(StrataInstall::Source, false, false), false, false),
        (on(StrataInstall::Terminal, true, false), false, false),
        (on(StrataInstall::Terminal, true, true), false, true),
        (on(StrataInstall::Terminal, false, true), false, false),
    ] {
        let state = format!(
            "{:?}, old folder {}, served {}",
            report.info.install, report.info.old_strata_folder, report.info.terminal_served
        );
        *s.pc.strata_info.lock().unwrap() = Some(report);
        let html = b.page("/admin/strata/version").await;
        assert_eq!(
            html.contains("Switch to Strata Terminal"),
            switch,
            "{state}: {html}"
        );
        assert_eq!(
            html.contains("Remove old Strata folder"),
            remove,
            "{state}: {html}"
        );
    }
}

#[tokio::test]
async fn the_switch_wakes_the_pc_and_is_recorded() {
    let dir = data_dir("switch");
    let s = answered(&dir, on(StrataInstall::Source, true, false), None).await;
    let mut b = restarted_asleep(&s).await;
    assert_eq!(page(&mut b).await["stale"], true);
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("Switch to Strata Terminal"), "{html}");

    let resp = b
        .call("strata-job", &[("kind", "switch_to_terminal")])
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) > 0);
    assert_eq!(last_body(&s), json!({"kind": "switch_to_terminal"}));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_switch_and_the_removal_land_in_the_history() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    b.call("strata-job", &[("kind", "switch_to_terminal")])
        .await;
    s.pc.update_job(|j| {
        j.state = StrataJobState::Failed;
        j.error = Some("The feed has no engine package for this PC.".into());
    });
    eventually("failure recorded", || {
        kinds(&s.state).contains(&EventKind::StrataJobFailed)
    })
    .await;
    b.call("strata-job", &[("kind", "remove_old_strata_folder")])
        .await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually("removal recorded", || {
        kinds(&s.state).contains(&EventKind::StrataJobDone)
    })
    .await;
    let details: Vec<_> = s
        .state
        .events
        .recent()
        .into_iter()
        .filter_map(|e| e.detail)
        .collect();
    for wanted in [
        "Switching to the Strata Terminal",
        "Switching to the Strata Terminal: The feed has no engine package for this PC.",
        "Removing the old Strata folder",
    ] {
        assert!(details.iter().any(|d| d == wanted), "{wanted}: {details:?}");
    }
}
