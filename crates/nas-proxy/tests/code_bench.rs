//! The code benchmark against a simulated runner (tools/code-bench/runner.py):
//! what the proxy sends it, the run's own key, and the kept results.

mod common;

use std::{
    sync::{Arc, Mutex},
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
use strata_nas_proxy::code_bench::KEY_NAME;

/// The runner as the proxy sees it.
#[derive(Clone, Default)]
struct Runner {
    /// The run's body as the proxy sent it.
    started: Arc<Mutex<Option<Value>>>,
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

fn run(state: &str) -> Value {
    json!({"id": 1, "model": "IQ3_XXS", "state": state, "error": null,
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
    *r.started.lock().unwrap() = Some(body);
    *r.current.lock().unwrap() = Some(run("running"));
    (StatusCode::ACCEPTED, Json(run("running"))).into_response()
}

async fn current(State(r): State<Runner>) -> Response {
    match r.current.lock().unwrap().clone() {
        Some(run) => Json(run).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn cancel(State(r): State<Runner>) -> Response {
    *r.cancels.lock().unwrap() += 1;
    *r.current.lock().unwrap() = Some(run("cancelled"));
    (StatusCode::ACCEPTED, Json(run("cancelled"))).into_response()
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
    (s, runner)
}

fn has_key(s: &Setup) -> bool {
    s.state.api_keys.list().iter().any(|k| k.name == KEY_NAME)
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
    assert!(runner.started.lock().unwrap().is_none());
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
    let body = runner.started.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "IQ3_XXS");
    assert_eq!(body["timeout_secs"], 1800);
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
    *runner.current.lock().unwrap() = Some(run("done"));
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
async fn a_runner_without_its_firewall_is_a_problem() {
    let (s, runner) = with_runner().await;
    *runner.problem.lock().unwrap() = Some("It needs the NET_ADMIN capability.".into());
    let mut b = logged_in(&s.url).await;
    let v = page(&mut b).await;
    assert_eq!(v["problem"], "It needs the NET_ADMIN capability.", "{v}");
    assert_eq!(b.call("code-bench-start", &[]).await.status(), 400);
    assert!(runner.started.lock().unwrap().is_none());
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
    assert!(runner.started.lock().unwrap().is_none());
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
    let body = runner.started.lock().unwrap().clone().unwrap();
    let key = body["api_key"].as_str().unwrap();
    assert_eq!(s.state.api_keys.check(key).as_deref(), Some(KEY_NAME));
}

#[tokio::test]
async fn a_run_the_proxy_lost_is_followed_again_and_can_be_cancelled() {
    let (s, runner) = with_runner().await;
    // The runner works on a run the proxy does not know, e.g. after a restart.
    *runner.current.lock().unwrap() = Some(run("running"));
    let mut b = logged_in(&s.url).await;
    let resp = b.call("code-bench-start", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("running already"));
    assert!(runner.started.lock().unwrap().is_none());
    assert_eq!(page(&mut b).await["run"]["state"], "running");
    assert_eq!(b.call("code-bench-cancel", &[]).await.status(), 200);
    assert_eq!(*runner.cancels.lock().unwrap(), 1);
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
    assert!(runner.started.lock().unwrap().is_none());
}

async fn eventually_for(limit: Duration, what: &str, check: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !check() {
        assert!(started.elapsed() < limit, "{what} nicht erreicht");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
