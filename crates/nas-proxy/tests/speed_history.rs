//! The speed benchmark's history: every run the proxy sees in the PC
//! service's reports, with what the model ran with, kept over a restart.

use crate::common;

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::Value;

/// The speed history on the Strata page.
async fn history(b: &mut Browser) -> Value {
    let resp = b.call("strata", &[]).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    v["benchmarks"].clone()
}

fn measured(model: &str, at: u64, decode: f64) -> StrataBenchmark {
    StrataBenchmark {
        at,
        decode_tps: Some(decode),
        context: Some(65536),
        version: Some("477ac13".into()),
        ..benchmark(model, &format!("{decode} Token/s"))
    }
}

fn report_with(benchmarks: Vec<StrataBenchmark>) -> StrataReport {
    let mut report = strata_info(true);
    report.info.benchmarks = benchmarks;
    report
}

/// The model tuned by hand: an active preset, an engine environment and an
/// engine option.
fn tuned(report: &mut StrataReport) {
    let m = &mut report.info.models[0];
    m.env = Some(BTreeMap::from([("GGML_CUDA_GRAPHS".into(), "1".into())]));
    m.user_args = Some(vec!["--batch-size 512".into()]);
    m.presets = Some(StrataPresets {
        latest: Some(StrataPreset {
            id: "p1".into(),
            name: "coding quick".into(),
            active: true,
            state: None,
            reasons: vec![],
            gain_pct: Some(12.0),
            settings: vec!["n_gpu_layers 40".into()],
            decode_tps: Some(43.0),
            base_tps: Some(38.5),
        }),
        other_context: false,
    });
}

#[tokio::test]
async fn runs_from_before_the_history_have_no_settings_and_new_ones_do() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() =
        Some(report_with(vec![measured("IQ3_XXS", 1_760_000_000, 38.5)]));
    let mut b = logged_in(&s.url).await;
    let h = history(&mut b).await;
    assert_eq!(h[0]["model"], "IQ3_XXS", "{h}");
    assert_eq!(h[0]["installed"], true, "{h}");
    assert_eq!(h[0]["runs"][0]["settings"], Value::Null, "{h}");

    let mut report = report_with(vec![measured("IQ3_XXS", 1_760_000_600, 43.0)]);
    tuned(&mut report);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let h = history(&mut b).await;
    // The same report again takes nothing new.
    let h_again = history(&mut b).await;
    assert_eq!(h, h_again);
    let runs = h[0]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{h}");
    assert_eq!(runs[0]["record"]["decode_tps"], 43.0, "{h}");
    let settings = &runs[0]["settings"];
    assert_eq!(settings["context"], 65536, "{h}");
    assert_eq!(settings["strata_version"], "477ac13", "{h}");
    assert_eq!(settings["preset"]["name"], "coding quick", "{h}");
    assert_eq!(settings["preset"]["settings"][0], "n_gpu_layers 40", "{h}");
    assert_eq!(settings["env"]["GGML_CUDA_GRAPHS"], "1", "{h}");
    assert_eq!(settings["engine_options"][0], "--batch-size 512", "{h}");
    assert_eq!(runs[1]["record"]["decode_tps"], 38.5, "{h}");
    assert_eq!(runs[1]["settings"], Value::Null, "{h}");

    // The model's row shows the newest result; the older run waits under it.
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("43 tok/s"), "{html}");
    assert!(!html.contains("38.5 tok/s"), "{html}");
    assert!(html.contains("aria-expanded"), "{html}");
}

#[tokio::test]
async fn a_failed_run_is_a_run_of_its_own() {
    let s = setup(true, |_| {}).await;
    let good = measured("IQ3_XXS", 1_760_000_000, 38.5);
    *s.pc.strata_info.lock().unwrap() = Some(report_with(vec![good.clone()]));
    let mut b = logged_in(&s.url).await;
    history(&mut b).await;

    // The PC service keeps the last measurement next to the error.
    let failed = StrataBenchmark {
        error: Some("Strata did not come up".into()),
        ..good
    };
    *s.pc.strata_info.lock().unwrap() = Some(report_with(vec![failed]));
    history(&mut b).await;
    let h = history(&mut b).await;
    let runs = h[0]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{h}");
    assert_eq!(runs[0]["failed"], true, "{h}");
    assert_eq!(runs[0]["record"]["error"], "Strata did not come up", "{h}");
    assert!(runs[0]["settings"].is_object(), "{h}");
    assert_eq!(runs[1]["failed"], false, "{h}");
    assert_eq!(runs[1]["record"]["error"], Value::Null, "{h}");

    // The row keeps the good result; the failure is under it.
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("38.5 tok/s"), "{html}");
    assert!(!html.contains("No result"), "{html}");
}

#[tokio::test]
async fn a_model_that_only_failed_shows_its_error() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(report_with(vec![StrataBenchmark {
        error: Some("out of memory".into()),
        ..benchmark("IQ3_XXS", "")
    }]));
    let mut b = logged_in(&s.url).await;
    let h = history(&mut b).await;
    assert_eq!(h[0]["runs"].as_array().unwrap().len(), 1, "{h}");
    assert_eq!(h[0]["runs"][0]["at"], 1_760_000_000u64, "{h}");
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("No result: out of memory"), "{html}");
    assert!(!html.contains("Make active"), "{html}");
}

#[tokio::test]
async fn runs_of_a_model_no_longer_installed_stay() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(report_with(vec![measured(
        "coder-IQ1_M",
        1_760_000_000,
        52.0,
    )]));
    let mut b = logged_in(&s.url).await;
    let h = history(&mut b).await;
    assert_eq!(h[0]["model"], "coder-IQ1_M", "{h}");
    assert_eq!(h[0]["installed"], false, "{h}");
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("Not installed"), "{html}");
    assert!(!html.contains("Make active"), "{html}");
}

#[tokio::test]
async fn the_history_is_kept_over_a_restart_and_shown_while_the_pc_sleeps() {
    let dir = data_dir("speed-history");
    let data = dir.clone();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    *s.pc.strata_info.lock().unwrap() =
        Some(report_with(vec![measured("IQ3_XXS", 1_760_000_000, 38.5)]));
    let mut b = logged_in(&s.url).await;
    history(&mut b).await;
    let mut report = report_with(vec![measured("IQ3_XXS", 1_760_000_600, 43.0)]);
    tuned(&mut report);
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let before = history(&mut b).await;

    s.pc.awake.store(false, Ordering::SeqCst);
    let mut b = logged_in_again(&serve_proxy(restarted(&s)).await).await;
    let after = history(&mut b).await;
    assert_eq!(after, before);
    let html = b.page("/admin/strata/version").await;
    assert!(html.contains("43 tok/s"), "{html}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}
