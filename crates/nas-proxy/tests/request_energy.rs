//! Request energy: the power plug read while requests run, each finished
//! request's energy in the request log, and each model's energy per 1000
//! tokens on the Strata page, against the simulated PC and its plug. The rig
//! reads the plug every 20 ms while a request runs.

use crate::common;

use std::{
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::*;
use serde_json::{Value, json};

const STREAM: &str = "/admin/api/requests-stream";
const CHAT: &str = "/v1/chat/completions";

/// A streamed answer of `model` with `tokens` output tokens: Strata pauses
/// `pause` after the first chunk, like a model at work.
fn answer(model: &str, tokens: u64) -> Vec<String> {
    let chunk = |delta: &str, finish: &str, extra: &str| {
        format!(
            r#"data: {{"id": "chatcmpl-1f0c", "object": "chat.completion.chunk", "created": 1761426000, "model": "{model}", "choices": [{{"index": 0, "delta": {delta}, "finish_reason": {finish}}}]{extra}}}"#
        ) + "\n\n"
    };
    let usage = format!(
        r#", "usage": {{"prompt_tokens": 500, "completion_tokens": {tokens}, "total_tokens": {}}}"#,
        500 + tokens
    );
    vec![
        chunk(r#"{"role": "assistant", "content": "Hel"}"#, "null", ""),
        chunk(r#"{"content": "lo"}"#, r#""stop""#, &usage),
        "data: [DONE]\n\n".into(),
    ]
}

fn chat() -> Value {
    json!({"model": "any", "stream": true,
           "messages": [{"role": "user", "content": "question"}]})
}

/// Sets up the power plug, reporting `watts`.
fn plug(s: &Setup, watts: f64) {
    *s.pc.plug_watts.lock().unwrap() = watts;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
}

/// Strata answers chats with `tokens` output tokens of `model` after `pause`.
fn answers(s: &Setup, model: &str, tokens: u64, pause: Duration) {
    s.pc.set_strata_stream(CHAT, answer(model, tokens), pause);
}

async fn ask(s: &Setup) {
    let resp = post_json(s, CHAT, chat()).await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
}

/// The next finished row the stream sends.
async fn next_row(live: &mut Live) -> Value {
    live.until("finished row", |m| {
        m["kind"] == "row" && m["row"]["state"] == "finished"
    })
    .await["row"]
        .clone()
}

async fn live(b: &Browser) -> Live {
    let mut live = Live::open_at(b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;
    live
}

/// One request's row, sent while the stream is open.
async fn one_row(s: &Setup, b: &Browser) -> Value {
    let mut live = live(b).await;
    ask(s).await;
    next_row(&mut live).await
}

/// `watts` over the time the row spent at Strata, in Wh.
fn expected_wh(row: &Value, watts: f64) -> f64 {
    let ms = row["duration_ms"].as_u64().unwrap() - row["wait_ms"].as_u64().unwrap();
    watts * ms as f64 / 3_600_000.0
}

fn assert_near(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= expected * 0.15,
        "{what}: {actual} not near {expected}"
    );
}

/// The history's loop, with `PLUG_SAMPLE_SECS` at an hour, has made its
/// first round without a plug and sleeps.
async fn history_loop_past_its_first_round() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn page(b: &mut Browser) -> Value {
    let resp = b.call("strata", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// The page's energy entry of `model`.
async fn energy_of(b: &mut Browser, model: &str) -> Value {
    let page = page(b).await;
    page["energy"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["model"] == model)
        .cloned()
        .unwrap_or_else(|| panic!("no energy of {model}: {}", page["energy"]))
}

/// A proxy whose PC service reports `IQ3_XXS` installed.
async fn with_model() -> Setup {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    s
}

#[tokio::test]
async fn a_finished_request_shows_its_energy_from_the_power_plug() {
    let s = setup(true, |_| {}).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));
    assert_near(wh, expected_wh(&row, 360.0), "energy");
    // At the rig's 30 ct/kWh.
    let eur = row["energy_eur"].as_f64().unwrap();
    assert!((eur - wh / 1000.0 * 0.30).abs() < 1e-12, "{row}");
}

#[tokio::test]
async fn the_energy_counts_from_strata_starting_not_from_the_wake_up() {
    let s = setup(false, |_| {}).await;
    plug(&s, 1000.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    // The simulated PC takes 300 ms to wake.
    assert!(row["wait_ms"].as_u64().unwrap() >= 250, "{row}");
    let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));
    assert_near(wh, expected_wh(&row, 1000.0), "energy");
}

#[tokio::test]
async fn overlapping_requests_split_the_shared_readings() {
    let s = setup(true, |_| {}).await;
    plug(&s, 400.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(600));
    let b = logged_in(&s.url).await;
    let mut live = live(&b).await;
    tokio::join!(ask(&s), ask(&s));
    for _ in 0..2 {
        let row = next_row(&mut live).await;
        let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));
        // Both ran nearly the whole time together: about half the draw each.
        assert_near(wh, expected_wh(&row, 200.0), "half the energy");
    }
}

#[tokio::test]
async fn a_request_without_a_power_plug_has_no_energy() {
    let s = setup(true, |_| {}).await;
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let mut b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert_eq!(row["outcome"]["kind"], "done", "{row}");
    assert_eq!(row["energy_wh"], Value::Null, "{row}");
    assert!(page(&mut b).await["energy"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_request_with_fewer_than_two_readings_has_no_energy() {
    let s = setup(true, |c| c.request_plug_sample = Duration::from_secs(10)).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert_eq!(row["outcome"]["kind"], "done", "{row}");
    assert_eq!(row["energy_wh"], Value::Null, "{row}");
}

#[tokio::test]
async fn a_request_without_output_tokens_has_no_energy() {
    let s = setup(true, |_| {}).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 0, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert_eq!(row["outcome"]["kind"], "done", "{row}");
    assert_eq!(row["energy_wh"], Value::Null, "{row}");
}

#[tokio::test]
async fn a_refused_or_cancelled_request_has_no_energy() {
    let s = setup(true, |_| {}).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_secs(2));
    let b = logged_in(&s.url).await;
    let mut live = live(&b).await;
    // The client hangs up while Strata is at work.
    let resp = post_json(&s, CHAT, chat()).await;
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(resp);
    let row = next_row(&mut live).await;
    assert_eq!(row["outcome"]["kind"], "cancelled_by_client", "{row}");
    assert_eq!(row["energy_wh"], Value::Null, "{row}");

    // Strata answers with an error.
    s.pc.chat_fails.store(true, Ordering::SeqCst);
    let failed = post_json(&s, CHAT, json!({"messages": []})).await;
    assert_eq!(failed.status(), 500);
    let row = next_row(&mut live).await;
    assert_eq!(row["outcome"]["kind"], "strata_error", "{row}");
    assert_eq!(row["energy_wh"], Value::Null, "{row}");
}

#[tokio::test]
async fn the_energy_in_euro_uses_todays_price() {
    let s = setup(true, |_| {}).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));
    s.state.settings.set("PRICE_CT_PER_KWH", "50").unwrap();
    let mut again = Live::open_at(&b, STREAM).await;
    let all = again.until("all", |m| m["kind"] == "all").await;
    let row = &all["rows"][0];
    let eur = row["energy_eur"].as_f64().unwrap();
    assert!((eur - wh / 1000.0 * 0.50).abs() < 1e-12, "{row}");
}

#[tokio::test]
async fn requests_do_not_add_samples_to_the_history() {
    let s = setup(true, |c| c.plug_sample = Duration::from_secs(3600)).await;
    history_loop_past_its_first_round().await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert!(row["energy_wh"].is_f64(), "{row}");
    let samples = s.state.events.samples(0, u64::MAX).await;
    assert!(samples.is_empty(), "{samples:?}");
}

#[tokio::test]
async fn the_plug_is_not_read_while_no_request_runs() {
    let s = setup(true, |c| c.plug_sample = Duration::from_secs(3600)).await;
    history_loop_past_its_first_round().await;
    plug(&s, 360.0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.plug_reads.load(Ordering::SeqCst), 0);

    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert!(row["energy_wh"].is_f64(), "{row}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = s.pc.plug_reads.load(Ordering::SeqCst);
    assert!(after >= 2, "{after} reads");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.plug_reads.load(Ordering::SeqCst), after);
}

#[tokio::test]
async fn request_energy_survives_a_restart() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("request-energy"))).await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));

    let state = restarted(&s);
    let url = serve_proxy(std::sync::Arc::clone(&state)).await;
    let b = logged_in_again(&url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    let all = live.until("all", |m| m["kind"] == "all").await;
    assert_eq!(all["rows"][0]["energy_wh"].as_f64(), Some(wh), "{all}");
    let averages = state.request_energy.averages(30.0, None).await;
    let all_time = averages[0].all_time.unwrap();
    assert_eq!(averages[0].model, "IQ3_XXS");
    assert!((all_time.wh_per_1000 - wh / 40.0 * 1000.0).abs() < 1e-9);
}

#[tokio::test]
async fn the_strata_page_shows_energy_per_1000_tokens_per_model() {
    let s = with_model().await;
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let mut b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    let wh = row["energy_wh"].as_f64().unwrap_or_else(|| panic!("{row}"));
    let energy = energy_of(&mut b, "IQ3_XXS").await;
    assert_eq!(energy["installed"], true, "{energy}");
    for period in ["last_30_days", "all_time"] {
        let average = &energy[period];
        let per_1000 = average["wh_per_1000"].as_f64().unwrap();
        assert!((per_1000 - wh / 40.0 * 1000.0).abs() < 1e-9, "{energy}");
        let eur = average["eur_per_1000"].as_f64().unwrap();
        assert!((eur - per_1000 / 1000.0 * 0.30).abs() < 1e-12, "{energy}");
        assert_eq!(average["requests"], 1, "{energy}");
    }
}

#[tokio::test]
async fn the_average_weights_requests_by_their_tokens() {
    let s = with_model().await;
    let mut b = logged_in(&s.url).await;
    s.state.request_energy.record(now(), "IQ3_XXS", 1.0, 100);
    s.state
        .request_energy
        .record(now(), "IQ3_XXS", 10.0, 10_000);
    let energy = energy_of(&mut b, "IQ3_XXS").await;
    let per_1000 = energy["all_time"]["wh_per_1000"].as_f64().unwrap();
    assert!(
        (per_1000 - 11.0 / 10_100.0 * 1000.0).abs() < 1e-9,
        "{energy}"
    );
    assert_eq!(energy["all_time"]["requests"], 2, "{energy}");
}

#[tokio::test]
async fn the_last_30_days_leave_out_older_days() {
    let s = with_model().await;
    let mut b = logged_in(&s.url).await;
    s.state
        .request_energy
        .record(now() - 40 * 86_400, "IQ3_XXS", 5.0, 1000);
    s.state.request_energy.record(now(), "IQ3_XXS", 1.0, 1000);
    let energy = energy_of(&mut b, "IQ3_XXS").await;
    assert_eq!(energy["last_30_days"]["wh_per_1000"], 1.0, "{energy}");
    assert_eq!(energy["last_30_days"]["requests"], 1, "{energy}");
    assert_eq!(energy["all_time"]["wh_per_1000"], 3.0, "{energy}");
    assert_eq!(energy["all_time"]["requests"], 2, "{energy}");

    // Only older days: nothing in the last 30.
    s.state
        .request_energy
        .record(now() - 40 * 86_400, "OTHER", 5.0, 1000);
    assert_eq!(
        energy_of(&mut b, "OTHER").await["last_30_days"],
        Value::Null
    );
}

#[tokio::test]
async fn the_daily_figures_outlive_the_request_log_limit() {
    let s = with_model().await;
    s.state.requests.set_limit(1);
    plug(&s, 360.0);
    answers(&s, "IQ3_XXS", 40, Duration::from_millis(300));
    let mut b = logged_in(&s.url).await;
    let mut live = live(&b).await;
    for _ in 0..2 {
        ask(&s).await;
        let row = next_row(&mut live).await;
        assert!(row["energy_wh"].is_f64(), "{row}");
    }
    let energy = energy_of(&mut b, "IQ3_XXS").await;
    assert_eq!(energy["all_time"]["requests"], 2, "{energy}");
}

#[tokio::test]
async fn an_alias_counts_for_its_model() {
    let s = with_model().await;
    *s.pc.listed_model.lock().unwrap() = Some("IQ3_XXS".into());
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let list = reqwest::Client::new()
        .get(format!("{}/v1/models", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    plug(&s, 360.0);
    // Strata answers with the alias the client asked for.
    answers(&s, "qwen", 40, Duration::from_millis(300));
    let mut b = logged_in(&s.url).await;
    let row = one_row(&s, &b).await;
    assert_eq!(row["model"], "qwen", "{row}");
    assert!(row["energy_wh"].is_f64(), "{row}");
    let page = page(&mut b).await;
    let models: Vec<&str> = page["energy"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["IQ3_XXS"], "{}", page["energy"]);
}

#[tokio::test]
async fn a_removed_model_keeps_its_figures_and_is_not_installed() {
    let s = with_model().await;
    let mut b = logged_in(&s.url).await;
    s.state
        .request_energy
        .record(now(), "coder-IQ1_M", 2.0, 1000);
    let energy = energy_of(&mut b, "coder-IQ1_M").await;
    assert_eq!(energy["installed"], false, "{energy}");
    assert_eq!(energy["all_time"]["wh_per_1000"], 2.0, "{energy}");
}
