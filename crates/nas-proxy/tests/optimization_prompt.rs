//! The optimization prompt: an English text per installed model for an AI
//! chat, built by the proxy from what the PC service reported.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::json;
use strata_pc_protocol::Incoming;

async fn prompt(b: &mut Browser, model: &str) -> String {
    let resp = b
        .call("strata-optimization-prompt", &[("model", model)])
        .await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

#[tokio::test]
async fn carries_the_model_and_the_task() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for part in [
        "https://github.com/Niko1221/Strata",
        "477ac13",
        "RAM: 64.0 GB",
        "IQ3_XXS",
        "81.0 GB",
        "user_args",
        "sampling",
    ] {
        assert!(text.contains(part), "{part} missing:\n{text}");
    }
}

#[tokio::test]
async fn needs_a_login() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = Browser::new(&s.url);
    let resp = b
        .call("strata-optimization-prompt", &[("model", "IQ3_XXS")])
        .await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn every_installed_model_offers_it() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report
        .info
        .models
        .push(model("coder-IQ1_M", Some("coder"), 30_000_000_000, false));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata").await;
    assert_eq!(html.matches(">Optimization prompt<").count(), 2, "{html}");
}

#[tokio::test]
async fn a_model_that_is_not_installed_has_none() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call("strata-optimization-prompt", &[("model", "IQ3_S")])
        .await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("IQ3_S is not installed")
    );
}

#[tokio::test]
async fn says_not_run_yet_without_benchmarks() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("- Speed benchmark: not run yet"), "{text}");
    assert!(text.contains("- Code benchmark: not run yet"), "{text}");
    // Live data needs no date.
    assert!(!text.contains("was read on"), "{text}");
}

#[tokio::test]
async fn carries_both_benchmarks_of_the_model() {
    let dir = data_dir("prompt-benchmarks");
    std::fs::create_dir_all(&dir).unwrap();
    let task = |name: &str, passed: u32| json!({"name": name, "title": name, "state": "done", "passed": passed, "total": 5});
    let runs = json!([
        {"model": "IQ3_XXS", "state": "done", "started": 1_760_000_000u64,
         "ended": 1_760_000_600u64, "tasks": [task("a", 5), task("b", 3)]},
        {"model": "coder-IQ1_M", "state": "done", "started": 1_760_000_000u64,
         "tasks": [task("a", 1)]}
    ]);
    std::fs::write(dir.join("code-bench.json"), runs.to_string()).unwrap();
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    let mut report = strata_info(true);
    let mut run = benchmark("IQ3_XXS", "38.5 Token/s");
    run.decode_tps = Some(38.5);
    run.prompt_tps = Some(1210.0);
    run.context = Some(65536);
    run.version = Some("477ac13".into());
    report.info.benchmarks = vec![benchmark("coder-IQ1_M", "90.0 Token/s"), run];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(
        text.contains(
            "- Speed benchmark: 38.5 tokens/s answering, 1210 tokens/s reading a 4K prompt, \
             at 65536 tokens context, Strata 477ac13, run on "
        ),
        "{text}"
    );
    assert!(!text.contains("90.0"), "{text}");
    assert!(
        text.contains("- Code benchmark: 8 of 10 hidden tests passed over 2 programming tasks"),
        "{text}"
    );
}

#[tokio::test]
async fn leaves_out_paths_keys_and_the_git_remote() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_key("0123456789abcdef0123456789abcdef");
    let mut report = strata_info(true);
    report.update = Some(Incoming {
        upstream: "fedcba9876543210".into(),
        total: 0,
        commits: vec![],
        files: vec![],
        remote: Some("https://example.org/someone/strata-fork.git".into()),
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    for secret in [
        "Strata-data",
        "0123456789abcdef",
        "strata-fork",
        s.key.as_str(),
        "10:20:30",
        "10-20-30",
    ] {
        assert!(!text.contains(secret), "{secret} in:\n{text}");
    }
}

#[tokio::test]
async fn a_sleeping_pc_gets_the_last_data_with_its_date_and_stays_asleep() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("IQ3_XXS"), "{text}");
    assert!(text.contains("This data was read on 20"), "{text}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn works_after_a_restart_while_the_pc_sleeps() {
    let dir = data_dir("prompt-restart");
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let url = serve_proxy(restarted(&s)).await;
    let mut b = logged_in_again(&url).await;
    let text = prompt(&mut b, "IQ3_XXS").await;
    assert!(text.contains("RAM: 64.0 GB"), "{text}");
    assert!(text.contains("This data was read on"), "{text}");
}
