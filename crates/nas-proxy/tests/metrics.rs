//! The metrics endpoint: `/metrics` and `/metrics/prometheus` scraped like
//! Prometheus does, against the simulated PC, Strata and power plug. The
//! proxy answers them itself: no wake-up, no activity, Strata's lines only
//! while it is up (docs/metrics.md).

use crate::common;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use common::*;
use serde_json::{Value, json};
use strata_nas_proxy::{
    api_keys::ModelName,
    events::{EventKind, Sample},
    lifecycle::PcState,
};
use strata_pc_protocol::{Cpu, Gpu, Hardware, StrataJobKind, StrataJobState};

const CHAT: &str = "/v1/chat/completions";
const STREAM: &str = "/admin/api/requests-stream";
/// Strata's own name of the model; the PC service names it `IQ3_XXS`.
const STRATA_NAME: &str = "qwen3.8-flash-next-iq3_s";

/// `GET path` with `key` as a Bearer token: status and body.
async fn get_with(s: &Setup, path: &str, key: Option<&str>) -> (u16, String) {
    let mut req = reqwest::Client::new().get(format!("{}{path}", s.url));
    if let Some(key) = key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

/// A scrape of `/metrics` with `key`, which must answer 200.
async fn scrape(s: &Setup, key: &str) -> String {
    let (status, text) = get_with(s, "/metrics", Some(key)).await;
    assert_eq!(status, 200, "{text}");
    text
}

/// The labels of one sample line, sorted.
fn labels_of(set: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = set;
    while let Some(eq) = rest.find("=\"") {
        let name = rest[..eq].trim_start_matches(',').to_string();
        let value_start = eq + 2;
        let mut end = value_start;
        let bytes = rest.as_bytes();
        while bytes[end] != b'"' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        out.push((name, rest[value_start..end].to_string()));
        rest = &rest[end + 1..];
    }
    out.sort();
    out
}

/// The value of the one line of `name` with exactly `labels`.
fn value(text: &str, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let mut want: Vec<(String, String)> = labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    want.sort();
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|line| {
            let (series, number) = line.rsplit_once(' ')?;
            let (n, set) = match series.split_once('{') {
                Some((n, set)) => (n, set.strip_suffix('}')?),
                None => (series, ""),
            };
            (n == name && labels_of(set) == want).then(|| number.parse().unwrap())
        })
}

/// Scrapes until the line of `name` with `labels` holds `expected`.
async fn until_value(s: &Setup, name: &str, labels: &[(&str, &str)], expected: f64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let text = scrape(s, &s.key).await;
        if value(&text, name, labels) == Some(expected) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name}{labels:?} never {expected}:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A streamed chat answer of `model` with the given token counts.
fn answer(model: &str, prompt: u64, cached: u64, output: u64) -> Vec<String> {
    let chunk = |delta: &str, finish: &str, extra: &str| {
        format!(
            r#"data: {{"id": "chatcmpl-1f0c", "object": "chat.completion.chunk", "created": 1761426000, "model": "{model}", "choices": [{{"index": 0, "delta": {delta}, "finish_reason": {finish}}}]{extra}}}"#
        ) + "\n\n"
    };
    let usage = format!(
        r#", "usage": {{"prompt_tokens": {prompt}, "completion_tokens": {output}, "total_tokens": {}, "prompt_tokens_details": {{"cached_tokens": {cached}}}}}"#,
        prompt + output
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

/// Strata answers chats as `STRATA_NAME` with these token counts.
fn answers(s: &Setup, prompt: u64, cached: u64, output: u64, pause: Duration) {
    s.pc.set_strata_stream(CHAT, answer(STRATA_NAME, prompt, cached, output), pause);
}

async fn ask_with(s: &Setup, key: &str) {
    let resp = post_json_to(&s.url, key, CHAT, chat()).await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
}

/// A proxy whose PC service reports `IQ3_XXS` installed and active, once
/// the proxy has its report.
async fn with_model(awake: bool, tweak: impl FnOnce(&mut strata_nas_proxy::Config)) -> Setup {
    let s = setup(awake, tweak).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    report_read(&mut logged_in(&s.url).await).await;
    s
}

/// The Strata page, which takes over the PC service's current report.
async fn report_read(b: &mut Browser) {
    assert_eq!(b.call("strata", &[]).await.status(), 200);
}

/// Sets up the power plug, reporting `watts`.
fn plug(s: &Setup, watts: f64) {
    *s.pc.plug_watts.lock().unwrap() = watts;
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn key(s: &Setup, name: &str, model_name: ModelName) -> String {
    s.state.api_keys.create(name, model_name).unwrap().0
}

fn assert_near(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= expected * 0.01,
        "{what}: {actual} not near {expected}"
    );
}

const REQUESTS: &str = "strata_proxy_requests_total";

// --- Access, wake-ups and activity ---

#[tokio::test]
async fn a_scrape_never_wakes_a_sleeping_pc() {
    let s = setup(false, |_| {}).await;
    for _ in 0..3 {
        scrape(&s, &s.key).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert!(!s.pc.up());
}

#[tokio::test]
async fn a_scrape_answers_while_the_pc_sleeps() {
    let s = setup(false, |_| {}).await;
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert_eq!(
        value(&text, "strata_proxy_pc_state", &[("state", "awake")]),
        Some(0.0),
        "{text}"
    );
}

#[tokio::test]
async fn strata_up_is_0_while_the_pc_sleeps() {
    let s = setup(false, |_| {}).await;
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert_eq!(
        value(&text, "strata_proxy_strata_up", &[]),
        Some(0.0),
        "{text}"
    );
}

#[tokio::test]
async fn scrapes_do_not_keep_the_pc_awake() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(400)).await;
    post_json(&s, "/v1/messages", json!({})).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while s.state.pc_state() != PcState::Asleep {
        assert!(
            std::time::Instant::now() < deadline,
            "the PC never went to sleep"
        );
        scrape(&s, &s.key).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_scrape_makes_no_request_log_row() {
    let s = setup(true, |_| {}).await;
    scrape(&s, &s.key).await;
    get_with(&s, "/metrics/prometheus", Some(&s.key)).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    let all = live.until("all", |m| m["kind"] == "all").await;
    assert_eq!(all["rows"], json!([]), "{all}");
}

#[tokio::test]
async fn a_scrape_without_an_api_key_gets_401() {
    let s = setup(true, |_| {}).await;
    let (status, text) = get_with(&s, "/metrics", None).await;
    assert_eq!(status, 401, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key", "{text}");
}

#[tokio::test]
async fn a_scrape_without_an_api_key_is_marked_unauthorized() {
    let s = setup(true, |_| {}).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/metrics", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()[strata_nas_proxy::proxy::STATUS_HEADER],
        "unauthorized"
    );
}

#[tokio::test]
async fn a_scrape_with_a_revoked_key_gets_401() {
    let s = setup(true, |_| {}).await;
    let (raw, info) = s
        .state
        .api_keys
        .create("Grafana", ModelName::Global)
        .unwrap();
    assert_eq!(get_with(&s, "/metrics", Some(&raw)).await.0, 200);
    s.state.api_keys.revoke(&info.name, &info.prefix).unwrap();
    assert_eq!(get_with(&s, "/metrics", Some(&raw)).await.0, 401);
}

#[tokio::test]
async fn a_scrape_accepts_the_key_as_x_api_key() {
    let s = setup(true, |_| {}).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/metrics", s.url))
        .header("x-api-key", &s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("strata_proxy_info"), "{text}");
}

// --- Paths and Strata's lines ---

#[tokio::test]
async fn metrics_prometheus_gives_the_same_answer_as_metrics() {
    let s = setup(false, |_| {}).await;
    until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    let (status, text) = get_with(&s, "/metrics/prometheus", Some(&s.key)).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text, scrape(&s, &s.key).await);
}

#[tokio::test]
async fn metrics_without_format_prometheus_gives_prometheus_text() {
    let s = setup(true, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    until_value(&s, "strata_proxy_strata_up", &[], 1.0).await;
    let text = scrape(&s, &s.key).await;
    assert!(
        text.contains("# TYPE strata_proxy_pc_state gauge"),
        "{text}"
    );
    assert!(text.contains("vllm:num_requests_running"), "{text}");
}

#[tokio::test]
async fn a_scrape_answers_with_prometheus_content_type() {
    let s = setup(true, |_| {}).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/metrics", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    let content_type = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "{content_type}"
    );
}

#[tokio::test]
async fn strata_lines_come_unchanged_while_strata_is_up() {
    let s = setup(true, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    let text = until_value(&s, "strata_proxy_strata_up", &[], 1.0).await;
    assert!(text.ends_with(METRICS_BUSY), "{text}");
}

#[tokio::test]
async fn strata_lines_are_missing_while_strata_does_not_answer() {
    let s = setup(true, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    s.pc.starting.store(true, Ordering::SeqCst);
    let text = scrape(&s, &s.key).await;
    assert!(!text.contains("vllm:"), "{text}");
}

#[tokio::test]
async fn strata_lines_are_missing_while_strata_answers_its_metrics_with_an_error() {
    let s = setup(true, |_| {}).await;
    // No metrics set: Strata answers them with 404 while `/health` is fine.
    let text = until_value(&s, "strata_proxy_strata_up", &[], 1.0).await;
    assert!(!text.contains("vllm:"), "{text}");
}

#[tokio::test]
async fn the_proxys_own_lines_come_while_strata_is_not_set_up() {
    let s = setup(true, |c| c.strata_url = None).await;
    let text = scrape(&s, &s.key).await;
    assert_eq!(
        value(&text, "strata_proxy_strata_up", &[]),
        Some(0.0),
        "{text}"
    );
}

// --- Strata's tool-call counters ---

const UNREAD: &str = "strata:tool_calls_unread_total";
const REPAIRED: &str = "strata:tool_args_repaired_total";

/// Strata's `/metrics/prometheus` with `unread` and `repaired` in place of
/// the fixture's 3 and 5.
fn llamacpp_with(unread: u64, repaired: u64) -> String {
    METRICS_LLAMACPP
        .replace(&format!("{UNREAD} 3\n"), &format!("{UNREAD} {unread}\n"))
        .replace(
            &format!("{REPAIRED} 5\n"),
            &format!("{REPAIRED} {repaired}\n"),
        )
}

/// Strata answers `/metrics/prometheus` with the fixture: 3 and 5.
async fn with_tool_counters() -> Setup {
    let s = setup(true, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    *s.pc.metrics_prometheus.lock().unwrap() = Some(METRICS_LLAMACPP.into());
    s
}

fn counters(text: &str) -> (Option<f64>, Option<f64>) {
    (value(text, UNREAD, &[]), value(text, REPAIRED, &[]))
}

/// Scrapes with `key` until both counters hold the given values.
async fn until_counters(s: &Setup, key: &str, unread: f64, repaired: f64) -> String {
    until_text(s, key, "the tool-call counters", |text| {
        counters(text) == (Some(unread), Some(repaired))
    })
    .await
}

#[tokio::test]
async fn the_tool_call_counters_show_stratas_values_while_strata_is_up() {
    let s = with_tool_counters().await;
    let text = until_counters(&s, &s.key, 3.0, 5.0).await;
    assert_eq!(
        value(&text, "strata_proxy_strata_up", &[]),
        Some(1.0),
        "{text}"
    );
}

#[tokio::test]
async fn the_tool_call_counters_keep_stratas_help_and_type() {
    let s = with_tool_counters().await;
    let text = until_counters(&s, &s.key, 3.0, 5.0).await;
    for family in [
        "# HELP strata:tool_calls_unread_total <tool_call> blocks returned as text \
         (an undeclared name or a form the template does not use)\n\
         # TYPE strata:tool_calls_unread_total counter\n",
        "# HELP strata:tool_args_repaired_total Tool call arguments (a JSON array or \
         object) that were damaged and read as written\n\
         # TYPE strata:tool_args_repaired_total counter\n",
    ] {
        assert!(text.contains(family), "{family}\n{text}");
    }
}

#[tokio::test]
async fn the_tool_call_counters_carry_no_labels() {
    let s = with_tool_counters().await;
    let text = until_counters(&s, &s.key, 3.0, 5.0).await;
    for name in [UNREAD, REPAIRED] {
        let lines: Vec<&str> = text.lines().filter(|l| l.starts_with(name)).collect();
        assert_eq!(lines.len(), 1, "{name}:\n{text}");
        assert!(lines[0].starts_with(&format!("{name} ")), "{text}");
    }
}

#[tokio::test]
async fn the_tool_call_counters_show_0_before_the_proxy_read_a_value() {
    let s = setup(false, |_| {}).await;
    *s.pc.metrics_prometheus.lock().unwrap() = Some(METRICS_LLAMACPP.into());
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert_eq!(counters(&text), (Some(0.0), Some(0.0)), "{text}");
}

#[tokio::test]
async fn the_tool_call_counters_keep_the_last_value_while_the_pc_sleeps() {
    let s = with_tool_counters().await;
    until_counters(&s, &s.key, 3.0, 5.0).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert_eq!(counters(&text), (Some(3.0), Some(5.0)), "{text}");
}

#[tokio::test]
async fn the_tool_call_counters_keep_the_last_value_while_strata_does_not_answer() {
    let s = with_tool_counters().await;
    until_counters(&s, &s.key, 3.0, 5.0).await;
    s.pc.starting.store(true, Ordering::SeqCst);
    // Strata's last answer to `/health` counts for 5 s (`HEALTH_CACHE`), as
    // long as `until_value` waits: give it more.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let text = loop {
        let text = scrape(&s, &s.key).await;
        if value(&text, "strata_proxy_strata_up", &[]) == Some(0.0) {
            break text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Strata never down:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(counters(&text), (Some(3.0), Some(5.0)), "{text}");
}

#[tokio::test]
async fn the_tool_call_counters_keep_the_last_value_when_metrics_prometheus_answers_an_error() {
    let s = with_tool_counters().await;
    until_counters(&s, &s.key, 3.0, 5.0).await;
    // Unset: Strata answers `/metrics/prometheus` with 404.
    *s.pc.metrics_prometheus.lock().unwrap() = None;
    let before = s.pc.metrics_prometheus_requests.load(Ordering::SeqCst);
    let text = scrape(&s, &s.key).await;
    assert!(s.pc.metrics_prometheus_requests.load(Ordering::SeqCst) > before);
    assert_eq!(counters(&text), (Some(3.0), Some(5.0)), "{text}");
}

#[tokio::test]
async fn the_tool_call_counters_keep_the_last_value_from_a_strata_that_does_not_print_them() {
    let s = with_tool_counters().await;
    until_counters(&s, &s.key, 3.0, 5.0).await;
    let older: String = METRICS_LLAMACPP
        .lines()
        .filter(|l| !l.contains("strata:tool_"))
        .map(|l| format!("{l}\n"))
        .collect();
    *s.pc.metrics_prometheus.lock().unwrap() = Some(older);
    let before = s.pc.metrics_prometheus_requests.load(Ordering::SeqCst);
    let text = scrape(&s, &s.key).await;
    assert!(s.pc.metrics_prometheus_requests.load(Ordering::SeqCst) > before);
    assert_eq!(counters(&text), (Some(3.0), Some(5.0)), "{text}");
}

#[tokio::test]
async fn the_tool_call_counters_follow_a_lower_value_from_strata() {
    let s = with_tool_counters().await;
    until_counters(&s, &s.key, 3.0, 5.0).await;
    *s.pc.metrics_prometheus.lock().unwrap() = Some(llamacpp_with(1, 2));
    until_counters(&s, &s.key, 1.0, 2.0).await;
}

#[tokio::test]
async fn a_key_that_hides_the_model_sees_the_same_tool_call_counters() {
    let s = with_tool_counters().await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    until_counters(&s, &hidden, 3.0, 5.0).await;
}

#[tokio::test]
async fn llamacpp_lines_never_reach_the_answer() {
    let s = with_tool_counters().await;
    let text = until_counters(&s, &s.key, 3.0, 5.0).await;
    assert!(!text.contains("llamacpp:"), "{text}");
}

#[tokio::test]
async fn the_other_strata_lines_of_metrics_prometheus_never_reach_the_answer() {
    let s = with_tool_counters().await;
    let text = until_counters(&s, &s.key, 3.0, 5.0).await;
    for name in [
        "strata:requests_total",
        "strata:drafts_offered_total",
        "strata:model_loaded",
    ] {
        assert!(!text.contains(name), "{name}:\n{text}");
    }
}

// --- Gauges ---

#[tokio::test]
async fn the_pc_state_gauge_marks_gaming_mode() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "gaming")], 1.0).await;
    assert_eq!(
        value(&text, "strata_proxy_pc_state", &[("state", "awake")]),
        Some(0.0),
        "{text}"
    );
}

#[tokio::test]
async fn the_tuning_gauge_is_1_during_a_tune() {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_job.lock().unwrap() = Some(job(StrataJobKind::Tune, StrataJobState::Running));
    s.pc.starting.store(true, Ordering::SeqCst);
    let text = until_value(&s, "strata_proxy_tuning", &[], 1.0).await;
    assert_eq!(
        value(&text, "strata_proxy_strata_up", &[]),
        Some(0.0),
        "{text}"
    );
}

#[tokio::test]
async fn strata_up_names_the_pc_services_active_model() {
    let s = with_model(true, |_| {}).await;
    until_value(&s, "strata_proxy_strata_up", &[("model", "IQ3_XXS")], 1.0).await;
}

#[tokio::test]
async fn the_power_gauge_shows_the_plugs_latest_reading() {
    let s = setup(true, |_| {}).await;
    plug(&s, 42.5);
    until_value(&s, "strata_proxy_power_watts", &[], 42.5).await;
    *s.pc.plug_watts.lock().unwrap() = 7.0;
    until_value(&s, "strata_proxy_power_watts", &[], 7.0).await;
}

#[tokio::test]
async fn there_is_no_power_gauge_without_a_power_plug() {
    let s = setup(true, |_| {}).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = scrape(&s, &s.key).await;
    assert!(!text.contains("strata_proxy_power_watts"), "{text}");
}

#[tokio::test]
async fn the_info_line_names_the_product_version() {
    let s = setup(true, |_| {}).await;
    let text = scrape(&s, &s.key).await;
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        value(&text, "strata_proxy_info", &[("version", version)]),
        Some(1.0),
        "{text}"
    );
}

// --- Hardware readings ---

/// Readings that differ from each other and from what a wrong unit gives.
fn readings() -> Hardware {
    Hardware {
        gpu: Some(Gpu {
            load_pct: Some(37.0),
            temp_c: Some(61.5),
            vram_used_bytes: Some(12_670_000_000),
            vram_total_bytes: Some(17_163_091_968),
            fan_rpm: Some(1450),
            power_pct: Some(71.0),
            power_watts: Some(182.5),
        }),
        cpu: Cpu {
            load_pct: Some(12.5),
            ram_used_bytes: Some(21_400_000_000),
            ram_total_bytes: Some(34_359_738_368),
        },
    }
}

/// An awake PC whose PC service reports `hardware`.
async fn reporting(hardware: Hardware) -> Setup {
    let s = setup(true, |_| {}).await;
    *s.pc.hardware.lock().unwrap() = Some(hardware);
    s
}

/// Scrapes with `key` until `done` holds for the answer, which it returns.
async fn until_text(s: &Setup, key: &str, what: &str, done: impl Fn(&str) -> bool) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let text = scrape(s, key).await;
        if done(&text) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never {what}:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn has_hardware_lines(text: &str) -> bool {
    text.contains("strata_proxy_gpu_")
        || text.contains("strata_proxy_cpu_")
        || text.contains("strata_proxy_ram_")
}

#[tokio::test]
async fn the_gpu_load_gauge_shows_the_last_reading_as_a_ratio() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_load_ratio", &[], 0.37).await;
}

#[tokio::test]
async fn the_gpu_temperature_gauge_shows_the_last_reading() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_temperature_celsius", &[], 61.5).await;
}

#[tokio::test]
async fn the_vram_used_gauge_shows_bytes_in_use() {
    let s = reporting(readings()).await;
    until_value(
        &s,
        "strata_proxy_gpu_vram_used_bytes",
        &[],
        12_670_000_000.0,
    )
    .await;
}

#[tokio::test]
async fn the_vram_total_gauge_shows_the_cards_vram() {
    let s = reporting(readings()).await;
    until_value(
        &s,
        "strata_proxy_gpu_vram_total_bytes",
        &[],
        17_163_091_968.0,
    )
    .await;
}

#[tokio::test]
async fn the_gpu_fan_gauge_shows_rpm() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_fan_rpm", &[], 1450.0).await;
}

#[tokio::test]
async fn a_stopped_fan_shows_0_rpm() {
    let mut hardware = readings();
    hardware.gpu.as_mut().unwrap().fan_rpm = Some(0);
    let s = reporting(hardware).await;
    until_value(&s, "strata_proxy_gpu_fan_rpm", &[], 0.0).await;
}

#[tokio::test]
async fn the_gpu_power_ratio_gauge_shows_the_share_of_the_limit() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_power_ratio", &[], 0.71).await;
}

#[tokio::test]
async fn the_gpu_power_watts_gauge_shows_the_last_reading() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_power_watts", &[], 182.5).await;
}

#[tokio::test]
async fn the_cpu_load_gauge_shows_the_last_reading_as_a_ratio() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_cpu_load_ratio", &[], 0.125).await;
}

#[tokio::test]
async fn the_ram_used_gauge_shows_bytes_in_use() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_ram_used_bytes", &[], 21_400_000_000.0).await;
}

#[tokio::test]
async fn the_ram_total_gauge_shows_the_pcs_ram() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_ram_total_bytes", &[], 34_359_738_368.0).await;
}

#[tokio::test]
async fn an_unknown_reading_leaves_its_line_out() {
    let mut hardware = readings();
    hardware.gpu.as_mut().unwrap().power_watts = None;
    let s = reporting(hardware).await;
    let text = until_value(&s, "strata_proxy_gpu_power_ratio", &[], 0.71).await;
    assert!(!text.contains("strata_proxy_gpu_power_watts"), "{text}");
}

#[tokio::test]
async fn no_gpu_lines_without_a_graphics_card() {
    let mut hardware = readings();
    hardware.gpu = None;
    let s = reporting(hardware).await;
    let text = until_value(&s, "strata_proxy_cpu_load_ratio", &[], 0.125).await;
    assert!(!text.contains("strata_proxy_gpu_"), "{text}");
}

#[tokio::test]
async fn no_hardware_lines_while_the_pc_sleeps() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_load_ratio", &[], 0.37).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert!(!has_hardware_lines(&text), "{text}");
}

#[tokio::test]
async fn hardware_lines_come_in_gaming_mode() {
    let s = reporting(readings()).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    until_text(&s, &s.key, "GPU load in gaming mode", |text| {
        value(text, "strata_proxy_pc_state", &[("state", "gaming")]) == Some(1.0)
            && value(text, "strata_proxy_gpu_load_ratio", &[]) == Some(0.37)
    })
    .await;
}

#[tokio::test]
async fn hardware_lines_disappear_once_the_last_answer_is_older_than_two_idle_checks() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_load_ratio", &[], 0.37).await;
    // The PC service stops answering while the PC and Strata stay up.
    s.pc.status_misses.store(usize::MAX / 2, Ordering::SeqCst);
    let text = until_text(&s, &s.key, "hardware lines gone", |text| {
        !has_hardware_lines(text)
    })
    .await;
    assert_eq!(
        value(&text, "strata_proxy_pc_state", &[("state", "awake")]),
        Some(1.0),
        "{text}"
    );
}

#[tokio::test]
async fn no_hardware_lines_from_a_pc_service_without_hardware_readings() {
    let s = reporting(readings()).await;
    until_value(&s, "strata_proxy_gpu_load_ratio", &[], 0.37).await;
    // The PC service goes back to a version before hardware readings.
    *s.pc.hardware.lock().unwrap() = None;
    let text = until_text(&s, &s.key, "hardware lines gone", |text| {
        !has_hardware_lines(text)
    })
    .await;
    assert_eq!(
        value(&text, "strata_proxy_pc_state", &[("state", "awake")]),
        Some(1.0),
        "{text}"
    );
}

#[tokio::test]
async fn a_key_that_hides_the_model_sees_the_same_hardware_lines() {
    let s = reporting(readings()).await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    until_text(&s, &hidden, "hardware lines for the hiding key", |text| {
        value(text, "strata_proxy_gpu_power_watts", &[]) == Some(182.5)
            && value(text, "strata_proxy_gpu_vram_used_bytes", &[]) == Some(12_670_000_000.0)
    })
    .await;
}

// --- Requests and tokens ---

#[tokio::test]
async fn an_answered_request_counts_as_answered_under_its_key_and_model() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    until_value(
        &s,
        REQUESTS,
        &[
            ("key", "Test"),
            ("model", "IQ3_XXS"),
            ("outcome", "answered"),
        ],
        1.0,
    )
    .await;
}

#[tokio::test]
async fn a_refused_request_counts_as_refused_without_a_model() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    assert_eq!(post_json(&s, CHAT, chat()).await.status(), 503);
    until_value(
        &s,
        REQUESTS,
        &[("key", "Test"), ("outcome", "refused")],
        1.0,
    )
    .await;
}

#[tokio::test]
async fn a_failed_request_counts_as_failed() {
    let s = with_model(true, |_| {}).await;
    s.pc.chat_fails.store(true, Ordering::SeqCst);
    let resp = post_json(&s, CHAT, json!({"model": "any", "messages": []})).await;
    assert_eq!(resp.status(), 500);
    resp.text().await.unwrap();
    until_value(
        &s,
        REQUESTS,
        &[("key", "Test"), ("model", "IQ3_XXS"), ("outcome", "failed")],
        1.0,
    )
    .await;
}

#[tokio::test]
async fn a_cancelled_request_counts_as_failed() {
    let s = with_model(true, |_| {}).await;
    // Strata works far longer than the test waits before it hangs up.
    answers(&s, 500, 0, 40, Duration::from_secs(10));
    let resp = post_json(&s, CHAT, chat()).await;
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(resp);
    until_value(
        &s,
        REQUESTS,
        &[("key", "Test"), ("model", "IQ3_XXS"), ("outcome", "failed")],
        1.0,
    )
    .await;
}

#[tokio::test]
async fn prompt_cached_and_output_tokens_add_up_per_key_and_model() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 100, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    ask_with(&s, &s.key).await;
    let labels = [("key", "Test"), ("model", "IQ3_XXS")];
    let text = until_value(&s, "strata_proxy_prompt_tokens_total", &labels, 1000.0).await;
    assert_eq!(
        value(&text, "strata_proxy_cached_tokens_total", &labels),
        Some(200.0),
        "{text}"
    );
    assert_eq!(
        value(&text, "strata_proxy_output_tokens_total", &labels),
        Some(80.0),
        "{text}"
    );
}

/// A code benchmark runner that keeps the run bodies the proxy sends.
async fn code_bench_runner() -> (String, Arc<Mutex<Vec<Value>>>) {
    use axum::{
        Json, Router,
        response::IntoResponse,
        routing::{get, post},
    };
    let started: Arc<Mutex<Vec<Value>>> = Arc::default();
    let runs = Arc::clone(&started);
    let current = Arc::clone(&started);
    let run = |model: &str| {
        json!({"id": 1, "model": model, "state": "running", "error": null,
               "started": 1_760_000_000u64, "ended": null, "tasks": []})
    };
    let router = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"opencode": "1.19.3", "problem": null})) }),
        )
        .route(
            "/runs",
            post(move |Json(body): Json<Value>| async move {
                let model = body["model"].as_str().unwrap_or("IQ3_XXS").to_string();
                runs.lock().unwrap().push(body);
                (axum::http::StatusCode::ACCEPTED, Json(run(&model)))
            }),
        )
        .route(
            "/runs/current",
            get(move || async move {
                if current.lock().unwrap().is_empty() {
                    return axum::http::StatusCode::NOT_FOUND.into_response();
                }
                Json(run("IQ3_XXS")).into_response()
            }),
        );
    (format!("http://{}", serve(router).await), started)
}

#[tokio::test]
async fn the_code_benchmarks_requests_count_under_its_key_name() {
    let (runner, started) = code_bench_runner().await;
    let s = with_model(true, |c| c.code_bench_url = Some(runner)).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    let mut b = logged_in_again(&s.url).await;
    let resp = b.call("code-bench-start", &[]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let run_key = started.lock().unwrap()[0]["api_key"]
        .as_str()
        .unwrap()
        .to_string();
    ask_with(&s, &run_key).await;
    until_value(
        &s,
        REQUESTS,
        &[
            ("key", strata_nas_proxy::code_bench::KEY_NAME),
            ("model", "IQ3_XXS"),
            ("outcome", "answered"),
        ],
        1.0,
    )
    .await;
}

#[tokio::test]
async fn every_key_sees_the_counters_of_all_keys() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    let laptop = key(&s, "Laptop", ModelName::Global);
    let grafana = key(&s, "Grafana", ModelName::Global);
    ask_with(&s, &s.key).await;
    ask_with(&s, &laptop).await;
    let labels = |k| [("key", k), ("model", "IQ3_XXS"), ("outcome", "answered")];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let text = scrape(&s, &grafana).await;
        if value(&text, REQUESTS, &labels("Test")) == Some(1.0)
            && value(&text, REQUESTS, &labels("Laptop")) == Some(1.0)
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{text}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn request_counters_survive_a_restart() {
    let dir = data_dir("metrics-restart");
    let s = with_model(true, |c| c.data_dir = Some(dir.clone())).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    let labels = [
        ("key", "Test"),
        ("model", "IQ3_XXS"),
        ("outcome", "answered"),
    ];
    until_value(&s, REQUESTS, &labels, 1.0).await;
    let state = restarted(&s);
    let url = serve_proxy(state).await;
    let again = Setup { url, ..s.clone() };
    let text = scrape(&again, &s.key).await;
    assert_eq!(value(&text, REQUESTS, &labels), Some(1.0), "{text}");
    assert_eq!(
        value(
            &text,
            "strata_proxy_output_tokens_total",
            &[("key", "Test"), ("model", "IQ3_XXS")]
        ),
        Some(40.0),
        "{text}"
    );
}

#[tokio::test]
async fn request_counters_start_at_zero_without_back_fill_from_the_request_log() {
    let dir = data_dir("metrics-no-back-fill");
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    // A finished row in the request log from before this release: only
    // the log has it, the request totals do not.
    s.state
        .events
        .database()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO requests (id, row) VALUES (1, ?1)",
            [
                json!({"id": 1, "at": now(), "key": "Test", "model": STRATA_NAME,
                    "api": "chat", "prompt_tokens": 500, "output_tokens": 40,
                    "wait_ms": 0, "duration_ms": 10, "state": "finished",
                    "outcome": {"kind": "done"}})
                .to_string(),
            ],
        )
        .unwrap();
    // The proxy of this release starts over that database.
    let url = serve_proxy(restarted(&s)).await;
    let again = Setup { url, ..s.clone() };
    let text = scrape(&again, &s.key).await;
    assert!(!text.contains(REQUESTS), "{text}");
}

#[tokio::test]
async fn request_totals_outlive_the_request_log_limit() {
    let s = with_model(true, |_| {}).await;
    s.state.requests.set_limit(1);
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    ask_with(&s, &s.key).await;
    until_value(
        &s,
        REQUESTS,
        &[
            ("key", "Test"),
            ("model", "IQ3_XXS"),
            ("outcome", "answered"),
        ],
        2.0,
    )
    .await;
}

// --- Energy and wake packets ---

#[tokio::test]
async fn request_energy_counts_in_joules_per_model() {
    let s = setup(true, |_| {}).await;
    plug(&s, 50.0);
    s.state.request_energy.record(now(), "IQ3_XXS", 2.0, 1000);
    s.state.request_energy.record(now(), "IQ3_XXS", 0.5, 1000);
    s.state.request_energy.record(now(), "Q4_K_M", 1.0, 1000);
    let text = scrape(&s, &s.key).await;
    let name = "strata_proxy_request_energy_joules_total";
    assert_eq!(
        value(&text, name, &[("model", "IQ3_XXS")]),
        Some(9000.0),
        "{text}"
    );
    assert_eq!(
        value(&text, name, &[("model", "Q4_K_M")]),
        Some(3600.0),
        "{text}"
    );
}

#[tokio::test]
async fn the_request_energy_counter_includes_days_before_this_release() {
    let s = setup(true, |_| {}).await;
    plug(&s, 50.0);
    s.state
        .request_energy
        .record(now() - 400 * 86_400, "IQ3_XXS", 1.0, 1000);
    let text = scrape(&s, &s.key).await;
    let name = "strata_proxy_request_energy_joules_total";
    assert_eq!(
        value(&text, name, &[("model", "IQ3_XXS")]),
        Some(3600.0),
        "{text}"
    );
}

/// A proxy whose plug loop waits a minute between rounds; its first round
/// at the start runs before the test sets up the plug.
async fn quiet_plug_loop() -> Setup {
    setup(true, |c| c.plug_sample = Duration::from_secs(60)).await
}

/// Sets up a power plug that gives no reading, so the plug loop saves no
/// sample of its own.
fn set_up_quiet_plug(s: &Setup) {
    s.pc.plug_down.store(true, Ordering::SeqCst);
    s.state.settings.set("SHELLY_URL", &s.plug).unwrap();
}

#[tokio::test]
async fn the_pc_energy_counter_adds_up_saved_plug_readings() {
    let s = quiet_plug_loop().await;
    let start = now() / 60 * 60 - 3600;
    for (i, watts) in [100.0, 50.0, 10.0].into_iter().enumerate() {
        s.state.events.record_sample(Sample {
            ts: start + 60 * i as u64,
            watts,
        });
    }
    set_up_quiet_plug(&s);
    let text = scrape(&s, &s.key).await;
    // Three minutes at 100, 50 and 10 W.
    let joules = value(&text, "strata_proxy_pc_energy_joules_total", &[]).unwrap();
    assert_near(joules, 160.0 * 60.0, "PC energy");
}

#[tokio::test]
async fn readings_in_one_plug_interval_count_once() {
    let s = quiet_plug_loop().await;
    let start = now() / 60 * 60 - 3600;
    s.state.events.record_sample(Sample {
        ts: start,
        watts: 100.0,
    });
    s.state.events.record_sample(Sample {
        ts: start + 30,
        watts: 50.0,
    });
    set_up_quiet_plug(&s);
    let text = scrape(&s, &s.key).await;
    // One minute at their mean of 75 W.
    let joules = value(&text, "strata_proxy_pc_energy_joules_total", &[]).unwrap();
    assert_near(joules, 75.0 * 60.0, "PC energy");
}

#[tokio::test]
async fn the_pc_energy_counter_grows_with_each_new_reading() {
    let s = quiet_plug_loop().await;
    let start = now() / 60 * 60 - 3600;
    s.state.events.record_sample(Sample {
        ts: start,
        watts: 100.0,
    });
    set_up_quiet_plug(&s);
    let first = scrape(&s, &s.key).await;
    let name = "strata_proxy_pc_energy_joules_total";
    assert_near(value(&first, name, &[]).unwrap(), 6000.0, "before");
    s.state.events.record_sample(Sample {
        ts: now(),
        watts: 20.0,
    });
    let second = scrape(&s, &s.key).await;
    assert_near(value(&second, name, &[]).unwrap(), 7200.0, "after");
}

#[tokio::test]
async fn there_are_no_energy_counters_without_a_power_plug() {
    let s = setup(true, |_| {}).await;
    s.state.request_energy.record(now(), "IQ3_XXS", 2.0, 1000);
    s.state.events.record_sample(Sample {
        ts: now() - 60,
        watts: 100.0,
    });
    let text = scrape(&s, &s.key).await;
    assert!(
        !text.contains("strata_proxy_request_energy_joules_total"),
        "{text}"
    );
    assert!(
        !text.contains("strata_proxy_pc_energy_joules_total"),
        "{text}"
    );
}

#[tokio::test]
async fn the_pc_energy_counter_survives_a_restart() {
    let dir = data_dir("metrics-pc-energy");
    let s = setup(true, |c| {
        c.data_dir = Some(dir.clone());
        c.plug_sample = Duration::from_secs(60);
    })
    .await;
    s.state.events.record_sample(Sample {
        ts: now() / 60 * 60 - 3600,
        watts: 100.0,
    });
    set_up_quiet_plug(&s);
    let state = restarted(&s);
    let url = serve_proxy(state).await;
    let again = Setup { url, ..s.clone() };
    let text = scrape(&again, &s.key).await;
    let joules = value(&text, "strata_proxy_pc_energy_joules_total", &[]).unwrap();
    assert_near(joules, 6000.0, "PC energy after the restart");
}

#[tokio::test]
async fn the_wake_packet_counter_counts_saved_wake_packets() {
    let s = setup(true, |_| {}).await;
    s.state.events.record(EventKind::WakeSent, None);
    s.state.events.record(EventKind::WakeSent, None);
    let text = scrape(&s, &s.key).await;
    assert_eq!(
        value(&text, "strata_proxy_wake_packets_total", &[]),
        Some(2.0),
        "{text}"
    );
}

// --- Keys that hide the model ---

#[tokio::test]
async fn a_key_that_hides_the_model_sees_the_public_name_in_strata_lines() {
    let s = setup(true, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_BUSY.into());
    until_value(&s, "strata_proxy_strata_up", &[], 1.0).await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    let text = scrape(&s, &hidden).await;
    assert!(
        text.contains(r#"vllm:num_requests_running{model_name="strata"} 1"#),
        "{text}"
    );
    assert!(!text.contains("Qwen3-30B-A3B"), "{text}");
}

#[tokio::test]
async fn a_key_that_hides_the_model_sees_the_public_name_in_the_proxys_lines() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    until_value(&s, "strata_proxy_strata_up", &[("model", "IQ3_XXS")], 1.0).await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    let text = scrape(&s, &hidden).await;
    let labels = [
        ("key", "Test"),
        ("model", "strata"),
        ("outcome", "answered"),
    ];
    assert_eq!(value(&text, REQUESTS, &labels), Some(1.0), "{text}");
    assert_eq!(
        value(&text, "strata_proxy_strata_up", &[("model", "strata")]),
        Some(1.0),
        "{text}"
    );
    assert!(!text.contains("IQ3_XXS"), "{text}");
}

#[tokio::test]
async fn models_under_one_public_name_add_up_into_one_line() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with(&s, &s.key).await;
    let mut info = strata_info(true);
    info.info.models = vec![
        model("IQ3_XXS", Some("qwen"), 81_000_000_000, false),
        model("Q4_K_M", Some("qwen"), 90_000_000_000, true),
    ];
    *s.pc.strata_info.lock().unwrap() = Some(info);
    report_read(&mut logged_in_again(&s.url).await).await;
    ask_with(&s, &s.key).await;
    until_value(
        &s,
        REQUESTS,
        &[
            ("key", "Test"),
            ("model", "Q4_K_M"),
            ("outcome", "answered"),
        ],
        1.0,
    )
    .await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    let text = scrape(&s, &hidden).await;
    let labels = [
        ("key", "Test"),
        ("model", "strata"),
        ("outcome", "answered"),
    ];
    assert_eq!(value(&text, REQUESTS, &labels), Some(2.0), "{text}");
}

#[tokio::test]
async fn the_image_model_keeps_its_name_for_a_key_that_hides_the_model() {
    let s = setup(true, |c| c.hide_model = true).await;
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let list = reqwest::Client::new()
        .get(format!("{}/v1/models", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    list.text().await.unwrap();
    let resp = post_json(
        &s,
        "/v1/images/generations",
        json!({"model": "flux-schnell"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
    let text = until_value(
        &s,
        REQUESTS,
        &[
            ("key", "Test"),
            ("model", "flux-schnell"),
            ("outcome", "answered"),
        ],
        1.0,
    )
    .await;
    assert!(!text.contains("Qwen3-30B-A3B"), "{text}");
}

// --- Expert tiers ---

const EXPERTS: &str = "strata_proxy_expert_tokens_total";

/// One of Strata's finished requests (`history.append`'s keys the proxy
/// reads); `None` is Strata's `null`.
fn entry(id: &str, output: u64, hit: Option<f64>, pcie: Option<f64>) -> Value {
    json!({"id": id, "finish": "stop", "prompt_tokens": 500, "output_tokens": output,
           "engine_generated": output, "hit_rate": hit, "pcie_share": pcie})
}

/// Waits until the simulated Strata answered `n` reads of its finished
/// requests.
async fn finished_reads(s: &Setup, n: usize) {
    let pc = s.pc.clone();
    eventually(
        &format!("{n} reads of Strata's finished requests"),
        move || pc.finished_requests_reads.load(Ordering::SeqCst) >= n,
    )
    .await;
}

/// A proxy with `IQ3_XXS` active that read Strata's finished requests once
/// while there were none, so the next ones count.
async fn primed(tweak: impl FnOnce(&mut strata_nas_proxy::Config)) -> Setup {
    let s = with_model(true, tweak).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    s.pc.set_strata_finished_requests(vec![]);
    ask_with(&s, &s.key).await;
    finished_reads(&s, 1).await;
    s
}

/// Strata lists `finished` (newest first) after the next request.
async fn ask_with_finished(s: &Setup, finished: Vec<Value>) {
    s.pc.set_strata_finished_requests(finished);
    ask_with(s, &s.key).await;
}

/// The tokens of `tier` over all models.
fn tier_sum(text: &str, tier: &str) -> f64 {
    text.lines()
        .filter(|l| l.starts_with(EXPERTS) && l.contains(&format!("tier=\"{tier}\"")))
        .map(|l| l.rsplit_once(' ').unwrap().1.parse::<f64>().unwrap())
        .sum()
}

/// Scrapes with `key` until the tiers over all models hold `[vram, pcie, cpu]`.
async fn until_tiers(s: &Setup, key: &str, expected: [f64; 3]) -> String {
    until_text(s, key, &format!("expert tiers {expected:?}"), |text| {
        ["vram", "pcie", "cpu"]
            .iter()
            .zip(expected)
            .all(|(tier, n)| (tier_sum(text, tier) - n).abs() < 1e-6)
    })
    .await
}

#[tokio::test]
async fn a_request_splits_its_output_tokens_into_vram_pcie_and_cpu() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(0.8), Some(0.25))]).await;
    until_tiers(&s, &s.key, [60.0, 25.0, 15.0]).await;
}

#[tokio::test]
async fn a_strata_request_counts_once_across_reads() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(1.0), Some(0.0)),
            entry("a", 100, Some(1.0), Some(0.0)),
        ],
    )
    .await;
    until_tiers(&s, &s.key, [110.0, 0.0, 0.0]).await;
}

#[tokio::test]
async fn requests_strata_held_at_the_first_read_are_not_counted() {
    let s = with_model(true, |_| {}).await;
    answers(&s, 500, 0, 40, Duration::ZERO);
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    finished_reads(&s, 1).await;
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(1.0), Some(0.0)),
            entry("a", 100, Some(1.0), Some(0.0)),
        ],
    )
    .await;
    until_tiers(&s, &s.key, [10.0, 0.0, 0.0]).await;
}

#[tokio::test]
async fn an_entry_without_a_hit_rate_counts_in_no_tier() {
    let s = primed(|_| {}).await;
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(0.0), Some(0.0)),
            entry("a", 100, None, Some(0.0)),
        ],
    )
    .await;
    until_tiers(&s, &s.key, [0.0, 0.0, 10.0]).await;
}

#[tokio::test]
async fn an_entry_without_a_pcie_share_counts_in_no_tier() {
    let s = primed(|_| {}).await;
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(0.0), Some(0.0)),
            entry("a", 100, Some(0.5), None),
        ],
    )
    .await;
    until_tiers(&s, &s.key, [0.0, 0.0, 10.0]).await;
}

#[tokio::test]
async fn expert_tokens_count_for_the_pc_services_model_name() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    let text = until_value(
        &s,
        EXPERTS,
        &[("model", "IQ3_XXS"), ("tier", "vram")],
        100.0,
    )
    .await;
    assert!(!text.contains(STRATA_NAME), "{text}");
}

/// The proxy started again over `s`'s data folder, on a new address.
async fn after_restart(s: &Setup) -> Setup {
    let state = restarted(s);
    let url = serve_proxy(Arc::clone(&state)).await;
    Setup {
        url,
        state,
        ..s.clone()
    }
}

#[tokio::test]
async fn expert_tokens_survive_a_restart() {
    let dir = data_dir("metrics-experts-restart");
    let s = primed(|c| c.data_dir = Some(dir.clone())).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(0.5), Some(0.0))]).await;
    until_tiers(&s, &s.key, [50.0, 0.0, 50.0]).await;
    let again = after_restart(&s).await;
    let text = scrape(&again, &s.key).await;
    assert_eq!(
        value(&text, EXPERTS, &[("model", "IQ3_XXS"), ("tier", "cpu")]),
        Some(50.0),
        "{text}"
    );
}

#[tokio::test]
async fn a_restart_counts_no_entry_again() {
    let dir = data_dir("metrics-experts-no-recount");
    let s = primed(|c| c.data_dir = Some(dir.clone())).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    let again = after_restart(&s).await;
    s.pc.set_strata_finished_requests(vec![
        entry("b", 10, Some(1.0), Some(0.0)),
        entry("a", 100, Some(1.0), Some(0.0)),
    ]);
    ask_with(&again, &s.key).await;
    until_tiers(&again, &s.key, [110.0, 0.0, 0.0]).await;
}

#[tokio::test]
async fn entries_after_a_strata_restart_count() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    // A new start of Strata: it lists only the requests since.
    ask_with_finished(&s, vec![entry("x", 20, Some(0.0), Some(1.0))]).await;
    until_tiers(&s, &s.key, [100.0, 20.0, 0.0]).await;
}

#[tokio::test]
async fn entries_a_failed_read_missed_count_with_the_next_read() {
    let s = primed(|_| {}).await;
    // Strata answers without its finished requests: the read fails.
    *s.pc.finished_requests.lock().unwrap() = None;
    ask_with(&s, &s.key).await;
    finished_reads(&s, 2).await;
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(1.0), Some(0.0)),
            entry("a", 100, Some(1.0), Some(0.0)),
        ],
    )
    .await;
    until_tiers(&s, &s.key, [110.0, 0.0, 0.0]).await;
}

#[tokio::test]
async fn expert_tokens_show_while_the_pc_sleeps() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    s.pc.awake.store(false, Ordering::SeqCst);
    let text = until_value(&s, "strata_proxy_pc_state", &[("state", "asleep")], 1.0).await;
    assert_eq!(
        value(&text, EXPERTS, &[("model", "IQ3_XXS"), ("tier", "vram")]),
        Some(100.0),
        "{text}"
    );
}

#[tokio::test]
async fn there_are_no_expert_token_lines_before_an_entry_counted() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, None, None)]).await;
    finished_reads(&s, 2).await;
    // Reads run one at a time: the third starts once the second is saved.
    ask_with(&s, &s.key).await;
    finished_reads(&s, 3).await;
    let text = scrape(&s, &s.key).await;
    assert!(!text.contains(EXPERTS), "{text}");
}

#[tokio::test]
async fn a_model_with_counted_entries_shows_all_three_tiers() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    let text = until_value(
        &s,
        EXPERTS,
        &[("model", "IQ3_XXS"), ("tier", "vram")],
        100.0,
    )
    .await;
    for tier in ["pcie", "cpu"] {
        assert_eq!(
            value(&text, EXPERTS, &[("model", "IQ3_XXS"), ("tier", tier)]),
            Some(0.0),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_scrape_never_reads_stratas_finished_requests() {
    let s = primed(|_| {}).await;
    for _ in 0..3 {
        scrape(&s, &s.key).await;
    }
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    // The priming request's read and this request's: none for the scrapes.
    assert_eq!(s.pc.finished_requests_reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_image_request_reads_no_finished_requests() {
    let s = primed(|_| {}).await;
    s.pc.list_alias_and_image.store(true, Ordering::SeqCst);
    let list = reqwest::Client::new()
        .get(format!("{}/v1/models", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    list.text().await.unwrap();
    s.pc.set_strata_finished_requests(vec![entry("a", 100, Some(1.0), Some(0.0))]);
    let resp = post_json(
        &s,
        "/v1/images/generations",
        json!({"model": "flux-schnell"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
    until_value(
        &s,
        REQUESTS,
        &[
            ("key", "Test"),
            ("model", "flux-schnell"),
            ("outcome", "answered"),
        ],
        1.0,
    )
    .await;
    // A read for the image request would count "a" for the image model.
    ask_with_finished(
        &s,
        vec![
            entry("b", 10, Some(1.0), Some(0.0)),
            entry("a", 100, Some(1.0), Some(0.0)),
        ],
    )
    .await;
    let text = until_value(
        &s,
        EXPERTS,
        &[("model", "IQ3_XXS"), ("tier", "vram")],
        110.0,
    )
    .await;
    assert!(!text.contains("model=\"flux-schnell\",tier"), "{text}");
}

#[tokio::test]
async fn a_key_that_hides_the_model_sees_the_public_name_in_expert_tokens() {
    let s = primed(|_| {}).await;
    ask_with_finished(&s, vec![entry("a", 100, Some(1.0), Some(0.0))]).await;
    until_tiers(&s, &s.key, [100.0, 0.0, 0.0]).await;
    let hidden = key(&s, "Phone", ModelName::Hide);
    let text = scrape(&s, &hidden).await;
    assert_eq!(
        value(&text, EXPERTS, &[("model", "strata"), ("tier", "vram")]),
        Some(100.0),
        "{text}"
    );
    assert!(!text.contains("IQ3_XXS"), "{text}");
}

const BROKEN: &str = "strata_proxy_request_body_broken_total";
/// A body cut right after a field name.
const CUT: &[u8] = br#"{"model": "x", "messages""#;

#[tokio::test]
async fn broken_bodies_are_counted_by_side() {
    let s = setup(true, |_| {}).await;
    for _ in 0..2 {
        let resp = post_raw(&s, "/v1/chat/completions", "application/json", CUT, None).await;
        assert_eq!(resp.status(), 400);
    }
    s.pc.set_strata_refuses_body("Expecting ',' delimiter: line 1 column 9 (char 8)");
    let resp = post_json(&s, "/v1/chat/completions", chat()).await;
    assert_eq!(resp.status(), 400);
    let text = scrape(&s, &s.key).await;
    assert_eq!(
        value(&text, BROKEN, &[("side", "arrival")]),
        Some(2.0),
        "{text}"
    );
    assert_eq!(
        value(&text, BROKEN, &[("side", "strata")]),
        Some(1.0),
        "{text}"
    );
    assert_eq!(
        value(&text, BROKEN, &[("side", "network")]),
        Some(0.0),
        "{text}"
    );
}

#[tokio::test]
async fn the_broken_body_counter_lists_every_side_at_zero() {
    let s = setup(true, |_| {}).await;
    let text = scrape(&s, &s.key).await;
    for side in ["arrival", "network", "strata"] {
        assert_eq!(
            value(&text, BROKEN, &[("side", side)]),
            Some(0.0),
            "{side}: {text}"
        );
    }
}

#[tokio::test]
async fn the_broken_body_counter_stays_over_a_restart() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("broken-counter"))).await;
    let resp = post_raw(&s, "/v1/chat/completions", "application/json", CUT, None).await;
    assert_eq!(resp.status(), 400);
    let url = serve_proxy(restarted(&s)).await;
    let again = Setup { url, ..s.clone() };
    let text = scrape(&again, &s.key).await;
    assert_eq!(
        value(&text, BROKEN, &[("side", "arrival")]),
        Some(1.0),
        "{text}"
    );
}
