//! The code benchmark against a simulated runner (tools/code-bench/runner.py):
//! what the proxy sends it, the run's own key, and the kept results.

use crate::common;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{code_bench, code_bench::KEY_NAME, state::AppState};

/// The runner as the proxy sees it.
#[derive(Clone, Default)]
struct Runner {
    /// The run bodies as the proxy sent them, in order.
    started: Arc<Mutex<Vec<Value>>>,
    /// What `GET /runs/current` answers.
    current: Arc<Mutex<Option<Value>>>,
    cancels: Arc<Mutex<u32>>,
    starts: Arc<Mutex<u32>>,
    /// Why the runner refuses runs, as its `/health` names it.
    problem: Arc<Mutex<Option<String>>>,
}

fn task(name: &str, passed: u32) -> Value {
    json!({"name": name, "title": name, "state": "done", "total_secs": 61.5, "steps": 7,
           "input_tokens": 9000, "output_tokens": 1200, "output_tps": 40.0,
           "passed": passed, "total": 8, "timed_out": false,
           "files": {"roman.py": "def to_roman(n): ..."}, "test_output": "Ran 8 tests", "error": null})
}

fn run(model: &str, state: &str) -> Value {
    json!({"id": 1, "model": model, "state": state, "error": null,
           "started": 1_760_000_000u64, "ended": null,
           "tasks": [task("roman", 8), task("csv-summary", 5), task("lru-cache", 0)]})
}

async fn health(State(r): State<Runner>) -> Json<Value> {
    Json(json!({"opencode": "1.19.3", "problem": *r.problem.lock().unwrap()}))
}

async fn start(State(r): State<Runner>, Json(body): Json<Value>) -> Response {
    // Slow enough for a second start to arrive meanwhile.
    tokio::time::sleep(Duration::from_millis(300)).await;
    *r.starts.lock().unwrap() += 1;
    let model = body["model"].as_str().unwrap_or("IQ3_XXS").to_string();
    r.started.lock().unwrap().push(body);
    *r.current.lock().unwrap() = Some(run(&model, "running"));
    (StatusCode::ACCEPTED, Json(run(&model, "running"))).into_response()
}

async fn current(State(r): State<Runner>) -> Response {
    match r.current.lock().unwrap().clone() {
        Some(run) => Json(run).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn cancel(State(r): State<Runner>) -> Response {
    *r.cancels.lock().unwrap() += 1;
    let model = r
        .current
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|run| run["model"].as_str())
        .unwrap_or("IQ3_XXS")
        .to_string();
    *r.current.lock().unwrap() = Some(run(&model, "cancelled"));
    (StatusCode::ACCEPTED, Json(run(&model, "cancelled"))).into_response()
}

async fn with_runner() -> (Setup, Runner) {
    let runner = Runner::default();
    let addr = serve(
        Router::new()
            .route("/health", get(health))
            .route("/runs", post(start))
            .route("/runs/current", get(current))
            .route("/runs/current/cancel", post(cancel))
            .with_state(runner.clone()),
    )
    .await;
    let s = setup(true, |c| c.code_bench_url = Some(format!("http://{addr}"))).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    s.state.code_bench.set_poll(Duration::from_millis(20));
    (s, runner)
}

/// A run over all models needs at least two installed models: the PC reports
/// `ids`, the first active.
async fn with_models(ids: &[&str]) -> (Setup, Runner) {
    let (s, runner) = with_runner().await;
    let mut info = strata_info(true);
    // The second model is active, so the run's restore differs from the first.
    info.info.models = ids
        .iter()
        .enumerate()
        .map(|(i, id)| model(id, Some("qwen"), 81_000_000_000, i == 1))
        .collect();
    *s.pc.strata_info.lock().unwrap() = Some(info);
    (s, runner)
}

/// A run as the runner would report one for `model`, with `passed` per task
/// and each task's time.
fn done_run(model: &str, passed: [u32; 3], secs: [f64; 3]) -> Value {
    let names = ["roman", "csv-summary", "lru-cache"];
    let tasks = (0..3)
        .map(|i| {
            let mut t = task(names[i], passed[i]);
            t["total_secs"] = json!(secs[i]);
            t
        })
        .collect::<Vec<_>>();
    json!({"id": 1, "model": model, "state": "done", "error": null,
           "started": 1_760_000_000u64, "ended": 1_760_000_100u64, "tasks": tasks})
}

/// The last Activate the PC service was asked for.
fn last_activate(s: &Setup) -> String {
    s.pc.job_bodies
        .lock()
        .unwrap()
        .last()
        .and_then(|b| b["model"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The last run the runner was asked for.
fn last_run(runner: &Runner) -> String {
    runner
        .started
        .lock()
        .unwrap()
        .last()
        .and_then(|b| b["model"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Drives one model of a run over all models: wait for its Activate, end the
/// job, wait for its run, then end that run.
async fn drive(s: &Setup, runner: &Runner, model: &str) {
    drive_model(s, runner, model, run(model, "done")).await;
}

/// Ends the run that the runner has started for `model` with `final`, and waits
/// until the proxy kept its result.
async fn drive_end(s: &Setup, runner: &Runner, model: &str, end_run: Value) {
    *runner.current.lock().unwrap() = Some(end_run);
    eventually_for(Duration::from_secs(10), "result for {model}", || {
        s.state.code_bench.has_result(model)
    })
    .await;
}

/// Activates `model` through the simulated PC, ends its job, waits for its run
/// to start, then ends that run.
async fn drive_model(s: &Setup, runner: &Runner, model: &str, end_run: Value) {
    for _ in 0..100 {
        if last_activate(s) == model {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(last_activate(s), model, "Activate {model} not asked for");
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    for _ in 0..100 {
        if last_run(runner) == model {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(last_run(runner), model, "run for {model} not started");
    drive_end(s, runner, model, end_run).await;
}

/// A model that does not load: its Activate ends with an error, so the run
/// goes on without a run for it.
async fn drive_load_fail(s: &Setup, model: &str) {
    for _ in 0..100 {
        if last_activate(s) == model {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(last_activate(s), model, "Activate {model} not asked for");
    s.pc.update_job(|j| {
        j.state = StrataJobState::Failed;
        j.error = Some("the model did not load".into());
    });
}

fn has_key(s: &Setup) -> bool {
    has_key_state(&s.state)
}

fn has_key_state(state: &Arc<AppState>) -> bool {
    state.api_keys.list().iter().any(|k| k.name == KEY_NAME)
}

async fn page(b: &mut Browser) -> Value {
    let resp = b.call("code-bench", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn needs_a_login() {
    let (s, runner) = with_runner().await;
    let mut b = Browser::new(&s.url);
    for name in ["code-bench", "code-bench-start", "code-bench-cancel"] {
        assert_eq!(b.call(name, &[]).await.status(), 401, "{name}");
    }
    assert!(runner.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_an_address_there_is_no_code_benchmark() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert!(
        v["problem"].as_str().unwrap().contains("CODE_BENCH_URL"),
        "{v}"
    );
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 400);
}

#[tokio::test]
async fn a_runner_that_does_not_answer_is_a_problem() {
    let s = setup(true, |c| {
        c.code_bench_url = Some("http://127.0.0.1:9".into())
    })
    .await;
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert!(
        v["problem"].as_str().unwrap().contains("does not answer"),
        "{v}"
    );
}

#[tokio::test]
async fn a_run_gets_the_tasks_the_active_model_and_a_key_of_its_own() {
    let (s, runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 200);
    let body = runner.started.lock().unwrap().first().cloned().unwrap();
    assert_eq!(body["model"], "IQ3_XXS");
    // The suggestion at the assumed speeds: no model has a benchmark.
    assert_eq!(body["timeout_secs"], 3360);
    let names: Vec<&str> = body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["roman", "csv-summary", "lru-cache"]);
    for t in body["tasks"].as_array().unwrap() {
        assert!(t["files"]["TASK.md"].as_str().unwrap().len() > 100, "{t}");
        assert!(t["files"]["example_test.py"].is_string(), "{t}");
        // The hidden tests are not among the files opencode sees.
        assert!(t["files"].get("hidden_test.py").is_none(), "{t}");
        assert!(
            t["hidden_test"].as_str().unwrap().contains("class Hidden"),
            "{t}"
        );
    }
    // The key works for the API while the run lasts.
    let key = body["api_key"].as_str().unwrap();
    assert!(has_key(&s));
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", s.url))
        .bearer_auth(key)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(page(&mut b).await["run"]["state"], "running");
    // A second run waits for the first.
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 400);
}

#[tokio::test]
async fn a_finished_run_is_kept_per_model_and_its_key_revoked() {
    let (s, runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    b.call("code-bench-start", &[]).await;
    *runner.current.lock().unwrap() = Some(run("IQ3_XXS", "done"));
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    let v = page(&mut b).await;
    assert!(v["run"].is_null(), "{v}");
    let result = &v["results"][0];
    assert_eq!(result["model"], "IQ3_XXS");
    assert_eq!(result["tasks"][1]["passed"], 5);
    assert!(!result["at_text"].as_str().unwrap().is_empty());
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("Code benchmark"), "{html}");
    assert!(html.contains("Total: 13/24 tests"), "{html}");
    assert!(html.contains("8/8"), "{html}");
    assert!(html.contains("def to_roman(n): ..."), "{html}");
}

#[tokio::test]
async fn cancelling_reaches_the_runner_and_keeps_no_result() {
    let (s, runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    b.call("code-bench-start", &[]).await;
    assert_eq!(b.call("code-bench-cancel", &[]).await.status(), 200);
    assert_eq!(*runner.cancels.lock().unwrap(), 1);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    let v = page(&mut b).await;
    assert_eq!(v["run"]["state"], "cancelled", "{v}");
    assert_eq!(v["results"], json!([]));
}

#[tokio::test]
async fn a_strata_job_waits_while_a_run_is_starting() {
    let (s, _runner) = with_runner().await;
    let mut a = logged_in(&s.url).await;
    let mut b = Browser {
        base: a.base.clone(),
        cookie: a.cookie.clone(),
    };
    // The runner takes 300 ms to answer the start; the job comes meanwhile.
    let (started, job) = tokio::join!(a.call("code-bench-start", &[]), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        b.call("strata-job", &[("kind", "benchmark")]).await
    });
    assert_eq!(started.status(), 200);
    assert_eq!(job.status(), 400);
}

#[tokio::test]
async fn a_runner_without_its_firewall_is_a_problem() {
    let (s, runner) = with_runner().await;
    *runner.problem.lock().unwrap() = Some("It needs the NET_ADMIN capability.".into());
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["problem"], "It needs the NET_ADMIN capability.", "{v}");
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 400);
    assert!(runner.started.lock().unwrap().is_empty());
    assert!(!has_key(&s));
}

#[tokio::test]
async fn a_key_someone_named_alike_stays_and_the_start_says_why() {
    let (s, runner) = with_runner().await;
    let (_, theirs) = s.state.api_keys.create("code benchmark").unwrap();
    let mut b = logged_in(&s.url).await;
    let resp = b.call("code-bench-start", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("Rename it"));
    assert!(
        s.state
            .api_keys
            .list()
            .iter()
            .any(|k| k.prefix == theirs.prefix)
    );
    assert!(runner.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn two_starts_at_once_make_one_run_with_a_working_key() {
    let (s, runner) = with_runner().await;
    let mut a = logged_in(&s.url).await;
    let mut b = Browser {
        base: a.base.clone(),
        cookie: a.cookie.clone(),
    };
    let (first, second) = tokio::join!(
        a.call("code-bench-start", &[]),
        b.call("code-bench-start", &[])
    );
    let mut codes = [first.status().as_u16(), second.status().as_u16()];
    codes.sort();
    assert_eq!(codes, [200, 400]);
    assert_eq!(*runner.starts.lock().unwrap(), 1);
    let body = runner.started.lock().unwrap().first().cloned().unwrap();
    let key = body["api_key"].as_str().unwrap();
    assert_eq!(s.state.api_keys.check(key).as_deref(), Some(KEY_NAME));
}

#[tokio::test]
async fn a_run_the_proxy_lost_is_followed_again_and_can_be_cancelled() {
    let (s, runner) = with_runner().await;
    // The runner works on a run the proxy does not know, e.g. after a restart.
    *runner.current.lock().unwrap() = Some(run("IQ3_XXS", "running"));
    let mut b = logged_in(&s.url).await;
    let resp = b.call("code-bench-start", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("running already"));
    assert!(runner.started.lock().unwrap().is_empty());
    assert_eq!(page(&mut b).await["run"]["state"], "running");
    assert_eq!(b.call("code-bench-cancel", &[]).await.status(), 200);
    assert_eq!(*runner.cancels.lock().unwrap(), 1);
}

#[tokio::test]
async fn the_update_summary_waits_for_the_code_benchmark() {
    let (s, _runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 200);
    let resp = b.call("strata-update-summary", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("code benchmark is running")
    );
    assert!(s.pc.chat_bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_strata_job_and_the_code_benchmark_wait_for_each_other() {
    let (s, _runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 200);
    let resp = b.call("strata-job", &[("kind", "benchmark")]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("code benchmark is running")
    );

    let (s, runner) = with_runner().await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        b.call("strata-job", &[("kind", "benchmark")])
            .await
            .status(),
        200
    );
    let resp = b.call("code-bench-start", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("Strata job is running"));
    assert!(runner.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_task_gets_the_suggested_or_the_saved_limit() {
    let (s, runner) = with_runner().await;
    {
        let mut report = s.pc.strata_info.lock().unwrap();
        let mut bench = benchmark("IQ3_XXS", "20 Token/s");
        bench.decode_tps = Some(20.0);
        report.as_mut().unwrap().info.benchmarks = vec![bench];
    }
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 200);
    // 50 000 / 20 × 2 = 5000 s, up to the full minute.
    let body = runner.started.lock().unwrap().first().cloned().unwrap();
    assert_eq!(body["timeout_secs"], 5040);
    b.call("code-bench-cancel", &[]).await;
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;

    assert_eq!(
        save_settings(&mut b, &[("CODE_BENCH_TASK_TIMEOUT_SECS", "45")])
            .await
            .status(),
        200
    );
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 200);
    let body = runner.started.lock().unwrap().last().cloned().unwrap();
    assert_eq!(body["timeout_secs"], 45);
}

async fn eventually_for(limit: Duration, what: &str, check: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !check() {
        assert!(started.elapsed() < limit, "{what} not reached");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- A run over all models ---

#[tokio::test]
async fn a_run_over_all_models_goes_through_them_and_brings_the_active_one_back() {
    let ids = ["IQ3_XXS", "coder-IQ1_M", "swift-IQ2_XS"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    for id in ids {
        drive(&s, &runner, id).await;
    }
    // The model active before the run (the second) is activated again.
    for _ in 0..100 {
        if last_activate(&s) == ids[1] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        last_activate(&s),
        ids[1],
        "the active model is not brought back"
    );
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    // The models in order, then the one active before the run.
    let order: Vec<String> =
        s.pc.job_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| b["model"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(order, vec![ids[0], ids[1], ids[2], ids[1]]);
    for id in ids {
        assert!(s.state.code_bench.has_result(id), "no result for {id}");
    }
    // One key for the whole run, gone at the end.
    assert!(!has_key(&s));
    let v = page(&mut b).await;
    assert!(v["all"].is_null(), "{v}");
}

#[tokio::test]
async fn a_model_that_does_not_load_gets_its_error_row_and_the_run_goes_on() {
    let ids = ["IQ3_XXS", "coder-IQ1_M", "swift-IQ2_XS"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    drive(&s, &runner, ids[0]).await;
    drive_load_fail(&s, ids[1]).await;
    // The error row for the model that did not load.
    eventually_for(Duration::from_secs(10), "error row", || {
        s.state.code_bench.error_row_for(ids[1])
    })
    .await;
    drive(&s, &runner, ids[2]).await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    let v = page(&mut b).await;
    let row = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["model"] == ids[1])
        .expect("no row for the model that failed");
    assert_eq!(row["state"], "failed", "{row}");
    assert!(
        row["error"].as_str().unwrap().contains("did not load"),
        "{row}"
    );
}

#[tokio::test]
async fn cancelling_a_run_over_all_models_keeps_finished_results_and_stops_the_rest() {
    let ids = ["IQ3_XXS", "coder-IQ1_M", "swift-IQ2_XS"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    drive(&s, &runner, ids[0]).await;
    // The second model is running when it is cancelled.
    for _ in 0..100 {
        if last_activate(&s) == ids[1] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "run for the second model", || {
        last_run(&runner) == ids[1]
    })
    .await;
    assert_eq!(b.call("code-bench-cancel", &[]).await.status(), 200);
    // The active model is brought back after the cancelled run ends: the run is
    // cancelled, so the model that ran now keeps no result, and the third model
    // never runs. The active model comes back as a new Activate job.
    eventually_for(
        Duration::from_secs(10),
        "the active model asked for again",
        || s.pc.job_bodies.lock().unwrap().len() == 3 && last_activate(&s) == ids[1],
    )
    .await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    // Only the finished model keeps a result; the third model never runs.
    assert!(
        s.state.code_bench.has_result(ids[0]),
        "the finished result is gone"
    );
    assert!(
        !s.state.code_bench.has_result(ids[2]),
        "a stopped model has a result"
    );
    assert!(
        !s.state.code_bench.error_row_for(ids[2]),
        "a stopped model got a row"
    );
    let order: Vec<String> =
        s.pc.job_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| b["model"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(order, vec![ids[0], ids[1], ids[1]]);

    // The next run over all models starts from the first model again.
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    eventually_for(
        Duration::from_secs(10),
        "the next run's first model",
        || s.pc.job_bodies.lock().unwrap().len() == 4 && last_activate(&s) == ids[0],
    )
    .await;
}

#[tokio::test]
async fn a_run_over_all_models_is_refused_with_the_reason() {
    let ids = ["IQ3_XXS", "coder-IQ1_M"];
    let (s, _runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    // Gaming mode.
    s.pc.gaming.store(true, Ordering::SeqCst);
    let resp = b.call("code-bench-start-all", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text().await.unwrap().contains("Gaming mode is on."),
        "gaming"
    );
    s.pc.gaming.store(false, Ordering::SeqCst);
    // A Strata job is running.
    *s.pc.strata_job.lock().unwrap() = Some(job(StrataJobKind::Benchmark, StrataJobState::Running));
    let resp = b.call("code-bench-start-all", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("A Strata job is running. Start the code benchmark once it ends."),
        "a running job"
    );
    *s.pc.strata_job.lock().unwrap() = None;
    // Fewer than two installed models.
    let only = with_models(&["IQ3_XXS"]).await;
    let mut b = logged_in(&only.0.url).await;
    let resp = b.call("code-bench-start-all", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("needs at least two models"),
        "fewer than two"
    );
}

#[tokio::test]
async fn results_are_sorted_by_tests_passed_then_the_time() {
    let ids = ["IQ3_XXS", "coder-IQ1_M"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    // The first model passes fewer tests, so it comes last.
    drive_model(
        &s,
        &runner,
        ids[0],
        done_run(ids[0], [4, 5, 0], [100.0, 100.0, 100.0]),
    )
    .await;
    drive_model(
        &s,
        &runner,
        ids[1],
        done_run(ids[1], [6, 6, 0], [40.0, 30.0, 30.0]),
    )
    .await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    let v = page(&mut b).await;
    let models: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["model"].as_str().unwrap())
        .collect();
    // More tests passed first.
    assert_eq!(models, vec![ids[1], ids[0]]);
    // Same tests passed: the shorter run first.
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    drive_model(
        &s,
        &runner,
        ids[0],
        done_run(ids[0], [3, 3, 0], [300.0, 100.0, 100.0]),
    )
    .await;
    drive_model(
        &s,
        &runner,
        ids[1],
        done_run(ids[1], [3, 3, 0], [60.0, 60.0, 60.0]),
    )
    .await;
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || !has_key(&s)).await;
    let v = page(&mut b).await;
    let models: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, vec![ids[1], ids[0]]);
}

#[tokio::test]
async fn a_run_over_all_models_refuses_a_strata_job_started_by_hand() {
    let ids = ["IQ3_XXS", "coder-IQ1_M"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    // While the first model is being activated, a job started by hand is refused.
    for _ in 0..100 {
        if last_activate(&s) == ids[0] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let resp = b.call("strata-job", &[("kind", "benchmark")]).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("A code benchmark is running. Strata jobs wait until it ends.")
    );
    // Ending the run's job keeps the run going.
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "run for the first model", || {
        last_run(&runner) == ids[0]
    })
    .await;
    b.call("code-bench-cancel", &[]).await;
}

#[tokio::test]
async fn after_a_restart_mid_run_the_current_run_is_followed_and_the_model_brought_back() {
    let ids = ["IQ3_XXS", "coder-IQ1_M"];
    let dir = data_dir("code-bench-restart");
    let runner = Runner::default();
    let addr = serve(
        Router::new()
            .route("/health", get(health))
            .route("/runs", post(start))
            .route("/runs/current", get(current))
            .route("/runs/current/cancel", post(cancel))
            .with_state(runner.clone()),
    )
    .await;
    let s = setup(true, |c| {
        c.code_bench_url = Some(format!("http://{addr}"));
        c.data_dir = Some(dir.clone());
    })
    .await;
    let mut info = strata_info(true);
    info.info.models = ids
        .iter()
        .enumerate()
        .map(|(i, id)| model(id, Some("qwen"), 81_000_000_000, i == 1))
        .collect();
    *s.pc.strata_info.lock().unwrap() = Some(info);
    s.state.code_bench.set_poll(Duration::from_millis(20));

    // The proxy is mid-run for the first model when it restarts: its key and the
    // model active before the run are in DATA_DIR, and the runner is working on
    // the first model's run.
    let (_, info) = s.state.api_keys.create(KEY_NAME).unwrap();
    std::fs::write(dir.join("code-bench-key"), info.prefix).unwrap();
    std::fs::write(dir.join("code-bench-restore"), ids[1]).unwrap();
    *runner.current.lock().unwrap() = Some(run(ids[0], "running"));

    // The proxy starts again with the same DATA_DIR: it follows the run that is
    // still going, keeps its result, brings the model active before the run
    // back and revokes the key. It does not resume the remaining models.
    let restarted = restarted(&s);
    assert!(
        has_key_state(&restarted),
        "the run's key is not kept over the restart"
    );
    code_bench::resume(&restarted);
    // Let the first poll see the run still going, so the proxy follows it; then
    // end that run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    *runner.current.lock().unwrap() = Some(run(ids[0], "done"));
    for _ in 0..100 {
        if last_activate(&s) == ids[1] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        last_activate(&s),
        ids[1],
        "the active model is not brought back"
    );
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "key revoked", || {
        !has_key_state(&restarted)
    })
    .await;
    assert!(
        restarted.code_bench.has_result(ids[0]),
        "the followed run's result is gone"
    );
    assert!(
        !restarted.code_bench.has_result(ids[1]),
        "a remaining model was resumed"
    );
    let order: Vec<String> =
        s.pc.job_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| b["model"].as_str().unwrap().to_string())
            .collect();
    // Only the model active before the run is activated again.
    assert_eq!(order, vec![ids[1]]);
    assert!(
        !dir.join("code-bench-restore").exists(),
        "the model to bring back is kept after the run"
    );
}

#[tokio::test]
async fn a_runner_that_stops_answering_gives_the_model_its_error_row_and_the_run_goes_on() {
    let ids = ["IQ3_XXS", "coder-IQ1_M", "swift-IQ2_XS"];
    let (s, runner) = with_models(&ids).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("code-bench-start-all", &[]).await.status(), 200);
    drive(&s, &runner, ids[0]).await;
    for _ in 0..100 {
        if last_activate(&s) == ids[1] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    eventually_for(Duration::from_secs(10), "run for the second model", || {
        last_run(&runner) == ids[1]
    })
    .await;
    // The runner forgets the run: the proxy stops asking after a few misses.
    *runner.current.lock().unwrap() = None;
    eventually_for(
        Duration::from_secs(10),
        "error row for the second model",
        || s.state.code_bench.error_row_for(ids[1]),
    )
    .await;
    drive(&s, &runner, ids[2]).await;
}
