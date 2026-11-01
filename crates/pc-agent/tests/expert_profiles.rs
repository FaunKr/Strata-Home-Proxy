//! Learned expert profiles through the service's HTTP interface: the jobs
//! that use, learn, stop and delete them, what lands in the run configs, the
//! hold for benchmarks and the clean stop of Strata while a profile learns.

use crate::common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

/// The Strata Terminal with IQ3_XXS (served) and coder-IQ1_M, each run
/// config naming the expert profile setup shipped in its Control folder.
fn installed(s: &Setup) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves.config("iq3_xxs", config(s, "a.gguf", "iq3_xxs.experts.bin"));
    leaves.config("coder-iq1_m", config(s, "b.gguf", "coder.experts.bin"));
    let terminal = Terminal::of(cfg);
    let served = leaves.installation().config_path("iq3_xxs");
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&served, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
            ..Default::default()
        })
        .unwrap();
    // No presets: the page asks Strata for them.
    s.fake.command(
        "strata.cmd presets",
        FakeCommand {
            lines: lines(&[r#"{"schema_version": 1, "ok": true, "active": null}"#]),
            ..FakeCommand::default()
        },
    );
    leaves
}

fn control(s: &Setup) -> PathBuf {
    s.agent
        .cfg
        .strata_home
        .join("components")
        .join("control-0.1.45")
}

fn shipped(s: &Setup, name: &str) -> String {
    control(s).join("data").join(name).display().to_string()
}

fn config(s: &Setup, gguf: &str, profile: &str) -> Value {
    let args = json!(["--native", gguf, "--expert-profile", shipped(s, profile)]);
    json!({"exe": "component:strata-engine", "args": args, "args_written": args,
        "cwd": control(s).display().to_string(), "port": 8080,
        "api_key": "key-0123456789abcdef"})
}

/// A run config as the service's own module reads it.
fn run_config(s: &Setup, id: &str) -> Value {
    let model = terminal_folder(s).installation().model(id).unwrap();
    Value::Object(model.fields().clone())
}

fn expert_arg(cfg: &Value) -> String {
    let args = cfg["args"].as_array().unwrap();
    let at = args.iter().position(|a| a == "--expert-profile").unwrap();
    args[at + 1].as_str().unwrap().to_string()
}

fn profile_file(s: &Setup, model: &str, name: &str) -> PathBuf {
    s.agent
        .cfg
        .strata_home
        .join("expert-profiles")
        .join(model)
        .join(format!("{name}.bin"))
}

/// The engine's save of profile `name`, as on a stop or after 5 minutes.
fn engine_saves(s: &Setup, model: &str, name: &str) {
    let path = profile_file(s, model, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "learned").unwrap();
}

async fn job(s: &Setup, body: Value) -> Value {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

async fn refused(s: &Setup, body: Value) -> (u16, String) {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    (
        code,
        answer["reason"].as_str().unwrap_or_default().to_string(),
    )
}

fn learn(model: &str, name: &str) -> Value {
    json!({"kind": "expert_profile", "model": model, "profile": name, "learn": true})
}

fn use_profile(model: &str, name: Option<&str>) -> Value {
    match name {
        Some(name) => json!({"kind": "expert_profile", "model": model, "profile": name}),
        None => json!({"kind": "expert_profile", "model": model}),
    }
}

fn expert_view(view: &Value, id: &str) -> Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))["expert_profiles"]
        .clone()
}

/// Strata started by the service with a process that runs until stopped,
/// so a stop can be a clean one.
fn strata_running(s: &Setup) {
    s.fake.command(
        "strata.cmd",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    s.agent.strata().start();
}

// --- run configs ---

#[tokio::test]
async fn learn_new_profile_writes_the_save_key_and_every_5_minutes() {
    let s = setup("expert-learn-new").await;
    installed(&s);
    let done = job(&s, learn("coder-IQ1_M", "coding")).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(
        cfg["expert_profile_save"],
        profile_file(&s, "coder-IQ1_M", "coding")
            .display()
            .to_string(),
        "{cfg}"
    );
    assert_eq!(cfg["expert_profile_save_every"], 5, "{cfg}");
}

#[tokio::test]
async fn learn_new_profile_keeps_the_shipped_expert_profile_in_args() {
    let s = setup("expert-learn-shipped").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(expert_arg(&cfg), shipped(&s, "coder.experts.bin"), "{cfg}");
}

#[tokio::test]
async fn learn_more_names_the_profile_file_in_args() {
    let s = setup("expert-learn-more").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    engine_saves(&s, "coder-IQ1_M", "coding");
    job(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    let done = job(&s, learn("coder-IQ1_M", "coding")).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    let file = profile_file(&s, "coder-IQ1_M", "coding")
        .display()
        .to_string();
    assert_eq!(expert_arg(&cfg), file, "{cfg}");
    assert_eq!(cfg["expert_profile_save"], file, "{cfg}");
    assert_eq!(cfg["args_written"], cfg["args"], "{cfg}");
}

#[tokio::test]
async fn use_learned_profile_names_its_file_in_args_without_save_key() {
    let s = setup("expert-use-learned").await;
    installed(&s);
    engine_saves(&s, "coder-IQ1_M", "coding");
    let done = job(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(
        expert_arg(&cfg),
        profile_file(&s, "coder-IQ1_M", "coding")
            .display()
            .to_string()
    );
    assert!(cfg.get("expert_profile_save").is_none(), "{cfg}");
    assert!(cfg.get("expert_profile_save_every").is_none(), "{cfg}");
}

#[tokio::test]
async fn use_standard_names_the_shipped_file_in_args() {
    let s = setup("expert-use-standard").await;
    installed(&s);
    engine_saves(&s, "coder-IQ1_M", "coding");
    job(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    let done = job(&s, use_profile("coder-IQ1_M", None)).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(expert_arg(&cfg), shipped(&s, "coder.experts.bin"), "{cfg}");
}

#[tokio::test]
async fn use_profile_never_saved_is_refused() {
    let s = setup("expert-use-unsaved").await;
    installed(&s);
    let (code, why) = refused(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    assert_eq!(code, 409, "{why}");
    assert!(why.contains("not saved"), "{why}");
}

#[tokio::test]
async fn learn_new_while_another_profile_of_the_model_learns_is_refused() {
    let s = setup("expert-learn-two").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    let (code, why) = refused(&s, learn("coder-IQ1_M", "chat")).await;
    assert_eq!(code, 409, "{why}");
    assert!(why.contains("coding"), "names the learning one: {why}");
}

#[tokio::test]
async fn profile_name_with_other_characters_is_refused() {
    let s = setup("expert-name-chars").await;
    installed(&s);
    let (code, why) = refused(&s, learn("coder-IQ1_M", "../evil")).await;
    assert_eq!(code, 400, "{why}");
}

#[tokio::test]
async fn profile_named_standard_is_refused() {
    let s = setup("expert-name-standard").await;
    installed(&s);
    let (code, why) = refused(&s, learn("coder-IQ1_M", "standard")).await;
    assert_eq!(code, 400, "{why}");
}

#[tokio::test]
async fn stop_learning_keeps_the_profile_active_and_drops_the_save_key() {
    let s = setup("expert-stop").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    engine_saves(&s, "coder-IQ1_M", "coding");
    let done = job(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert!(cfg.get("expert_profile_save").is_none(), "{cfg}");
    let profiles = expert_view(&get(&s, "/strata").await, "coder-IQ1_M");
    assert_eq!(profiles["active"], "coding", "{profiles}");
    assert_eq!(profiles["learning"], false, "{profiles}");
}

#[tokio::test]
async fn profile_change_for_the_active_model_restarts_strata_once() {
    let s = setup("expert-restart-active").await;
    installed(&s);
    let done = job(&s, learn("IQ3_XXS", "coding")).await;
    assert_eq!(done["state"], "done", "{done}");
    eventually("Strata restarted", || {
        s.fake.starts.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn profile_change_for_another_model_restarts_nothing() {
    let s = setup("expert-restart-other").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn delete_active_profile_switches_back_to_standard() {
    let s = setup("expert-delete-active").await;
    installed(&s);
    engine_saves(&s, "coder-IQ1_M", "coding");
    job(&s, use_profile("coder-IQ1_M", Some("coding"))).await;
    let body =
        json!({"kind": "delete_expert_profile", "model": "coder-IQ1_M", "profile": "coding"});
    let done = job(&s, body).await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(expert_arg(&cfg), shipped(&s, "coder.experts.bin"), "{cfg}");
    assert!(!profile_file(&s, "coder-IQ1_M", "coding").exists());
}

#[tokio::test]
async fn delete_learning_profile_ends_learning_and_removes_the_file() {
    let s = setup("expert-delete-learning").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    engine_saves(&s, "coder-IQ1_M", "coding");
    let body =
        json!({"kind": "delete_expert_profile", "model": "coder-IQ1_M", "profile": "coding"});
    assert_eq!(job(&s, body).await["state"], "done");
    let cfg = run_config(&s, "coder-iq1_m");
    assert!(cfg.get("expert_profile_save").is_none(), "{cfg}");
    assert!(!profile_file(&s, "coder-IQ1_M", "coding").exists());
    let profiles = expert_view(&get(&s, "/strata").await, "coder-IQ1_M");
    assert_eq!(profiles["learning"], false, "{profiles}");
}

#[tokio::test]
async fn delete_standard_is_refused() {
    let s = setup("expert-delete-standard").await;
    installed(&s);
    let body =
        json!({"kind": "delete_expert_profile", "model": "coder-IQ1_M", "profile": "Standard"});
    let (code, why) = refused(&s, body).await;
    assert_eq!(code, 400, "{why}");
}

#[tokio::test]
async fn delete_model_deletes_its_expert_profiles() {
    let s = setup("expert-delete-model").await;
    installed(&s);
    engine_saves(&s, "coder-IQ1_M", "coding");
    let done = job(&s, json!({"kind": "delete", "model": "coder-IQ1_M"})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(
        !profile_file(&s, "coder-IQ1_M", "coding")
            .parent()
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn setup_run_keeps_the_learning_save_key() {
    let s = setup("expert-setup-keeps").await;
    let leaves = installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    // Setup writes the run config anew, without the save key.
    let path = leaves.installation().config_path("coder-iq1_m");
    s.fake.command(
        "python",
        FakeCommand {
            writes: vec![(path, config(&s, "b.gguf", "coder.experts.bin").to_string())],
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "set_pictures", "model": "coder-IQ1_M", "on": false}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let cfg = run_config(&s, "coder-iq1_m");
    assert_eq!(
        cfg["expert_profile_save"],
        profile_file(&s, "coder-IQ1_M", "coding")
            .display()
            .to_string(),
        "{cfg}"
    );
}

#[tokio::test]
async fn run_config_with_expert_profile_save_by_hand_reports_by_hand() {
    let s = setup("expert-by-hand").await;
    let leaves = installed(&s);
    let mut cfg = config(&s, "b.gguf", "coder.experts.bin");
    cfg["expert_profile_save"] = json!("D:/mine/coding.bin");
    leaves.config("coder-iq1_m", cfg);
    let profiles = expert_view(&get(&s, "/strata").await, "coder-IQ1_M");
    assert_eq!(profiles["by_hand"], true, "{profiles}");
}

#[tokio::test]
async fn profile_job_for_a_model_set_by_hand_is_refused() {
    let s = setup("expert-by-hand-refused").await;
    let leaves = installed(&s);
    let mut cfg = config(&s, "b.gguf", "coder.experts.bin");
    cfg["user_args"] = json!(["--expert-profile", "D:/mine/coding.bin"]);
    leaves.config("coder-iq1_m", cfg);
    let (code, why) = refused(&s, learn("coder-IQ1_M", "coding")).await;
    assert_eq!(code, 409, "{why}");
    assert_eq!(why, "Expert profile set by hand in the run config");
}

#[tokio::test]
async fn report_lists_profiles_with_size_and_save_time() {
    let s = setup("expert-report").await;
    installed(&s);
    engine_saves(&s, "coder-IQ1_M", "coding");
    let profiles = expert_view(&get(&s, "/strata").await, "coder-IQ1_M");
    assert_eq!(profiles["active"], Value::Null, "Standard: {profiles}");
    let listed = &profiles["profiles"][0];
    assert_eq!(listed["name"], "coding", "{profiles}");
    assert_eq!(listed["bytes"], 7, "{profiles}");
    assert!(listed["saved"].as_u64().unwrap() > 0, "{profiles}");
}

// --- learning seconds ---

#[tokio::test]
async fn status_reports_learning_seconds_while_strata_answers() {
    let s = setup("expert-seconds").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    s.agent.learning_tick().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    s.agent.learning_tick().await;
    let status = get(&s, "/status").await;
    let learning = &status["expert_learning"];
    assert_eq!(learning["model"], "IQ3_XXS", "{status}");
    assert_eq!(learning["profile"], "coding", "{status}");
    assert_eq!(learning["held"], false, "{status}");
    assert_eq!(learning["secs"], 1, "{status}");
}

#[tokio::test]
async fn learning_seconds_survive_a_service_restart() {
    let s = setup("expert-seconds-restart").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    s.agent.learning_tick().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    s.agent.learning_tick().await;
    let again = s.restarted();
    let status = again.status().await;
    let learning = status.expert_learning.expect("still learning");
    assert!(learning.secs > 0, "seconds kept: {}", learning.secs);
}

// --- held for benchmarks ---

/// A benchmark that answers after `pause`, so its run can be looked at.
fn slow_bench(s: &Setup, pause: Duration, hang: bool) {
    s.fake.command(
        "strata.cmd bench",
        FakeCommand {
            lines: lines(&[r#"{"schema_version": 1, "ok": true, "path": "C:\\bench\\results.json", "status": "complete", "quality": {}, "summaries": [{"phase": "performance", "case_id": "short", "concurrency": 1, "metric": "decode_tps", "unit": "tokens/s", "median": 38.54, "min": 37.1, "max": 39.2, "n": 3, "included_request_ids": [], "excluded": []}]}"#]),
            pause,
            hang,
            ..FakeCommand::default()
        },
    );
}

fn saves(s: &Setup, id: &str) -> bool {
    run_config(s, id).get("expert_profile_save").is_some()
}

#[tokio::test]
async fn benchmark_runs_without_the_save_key_and_puts_it_back() {
    let s = setup("expert-bench").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    slow_bench(&s, Duration::from_millis(1500), false);
    assert_eq!(
        post(&s, "/strata/jobs", json!({"kind": "benchmark"}))
            .await
            .0,
        202
    );
    eventually("the save key dropped", || !saves(&s, "iq3_xxs")).await;
    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(saves(&s, "iq3_xxs"), "{}", run_config(&s, "iq3_xxs"));
}

#[tokio::test]
async fn compare_models_runs_without_the_save_key_and_puts_it_back() {
    let s = setup("expert-compare").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    slow_bench(&s, Duration::from_millis(1500), false);
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "compare_models"})).await;
    assert_eq!(code, 202, "{answer}");
    eventually("the save key dropped", || !saves(&s, "iq3_xxs")).await;
    let done = job_until(&s, "end of the comparison", |j| j["state"] != "running").await;
    assert_ne!(done["state"], "running", "{done}");
    assert!(saves(&s, "iq3_xxs"), "{}", run_config(&s, "iq3_xxs"));
}

#[tokio::test]
async fn cancelled_benchmark_puts_the_save_key_back() {
    let s = setup("expert-bench-cancel").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    slow_bench(&s, Duration::ZERO, true);
    assert_eq!(
        post(&s, "/strata/jobs", json!({"kind": "benchmark"}))
            .await
            .0,
        202
    );
    eventually("the save key dropped", || !saves(&s, "iq3_xxs")).await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    let done = finished(&s).await;
    assert_eq!(done["state"], "cancelled", "{done}");
    eventually("the save key back", || saves(&s, "iq3_xxs")).await;
}

#[tokio::test]
async fn hold_learning_restarts_without_the_save_key() {
    let s = setup("expert-hold").await;
    installed(&s);
    job(&s, learn("IQ3_XXS", "coding")).await;
    let starts = s.fake.starts.load(Ordering::SeqCst);
    let done = job(&s, json!({"kind": "hold_learning", "on": true})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(!saves(&s, "iq3_xxs"));
    eventually("Strata restarted", || {
        s.fake.starts.load(Ordering::SeqCst) == starts + 1
    })
    .await;
    assert_eq!(get(&s, "/status").await["expert_learning"]["held"], true);
    job(&s, json!({"kind": "hold_learning", "on": false})).await;
    assert!(saves(&s, "iq3_xxs"));
}

#[tokio::test]
async fn hold_learning_without_a_learning_profile_restarts_nothing() {
    let s = setup("expert-hold-none").await;
    installed(&s);
    let done = job(&s, json!({"kind": "hold_learning", "on": true})).await;
    assert_eq!(done["state"], "done", "{done}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

// --- the clean stop ---

#[tokio::test]
async fn stop_while_learning_sends_ctrl_break_first() {
    let s = setup("expert-clean-stop").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    strata_running(&s);
    assert!(s.agent.strata().stop_cleanly().await);
    assert_eq!(s.fake.breaks.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0, "no hard kill");
}

#[tokio::test]
async fn stop_while_learning_ends_hard_after_the_clean_stop_wait() {
    let s = setup("expert-clean-stop-wait").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    strata_running(&s);
    s.fake.strata_ignores_break.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    s.agent.strata().stop_cleanly().await;
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "clean_stop_secs waited"
    );
    assert_eq!(s.fake.breaks.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1, "then a hard kill");
}

#[tokio::test]
async fn stop_without_learning_ends_hard_at_once() {
    let s = setup("expert-hard-stop").await;
    installed(&s);
    strata_running(&s);
    s.agent.strata().stop_cleanly().await;
    assert_eq!(s.fake.breaks.load(Ordering::SeqCst), 0);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forced_restart_while_learning_ends_hard_at_once() {
    let s = setup("expert-forced").await;
    installed(&s);
    job(&s, learn("coder-IQ1_M", "coding")).await;
    strata_running(&s);
    let (code, answer) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{answer}");
    assert_eq!(s.fake.breaks.load(Ordering::SeqCst), 0);
    assert!(s.fake.stops.load(Ordering::SeqCst) >= 1);
}
