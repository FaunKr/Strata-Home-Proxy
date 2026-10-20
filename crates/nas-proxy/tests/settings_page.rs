//! The settings page: every value with its origin, only after login.

use crate::common;

use common::*;
use serde_json::Value;
use strata_nas_proxy::{AppState, Config, Settings, api_router, config::FIELDS};

async fn settings(b: &mut Browser) -> Value {
    let resp = b.call("settings", &[]).await;
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

fn entry<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == key)
        .unwrap_or_else(|| panic!("{key} is missing"))
}

#[tokio::test]
async fn settings_need_a_login() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(b.call("settings", &[]).await.status(), 401);
    let html = b.page("/admin/system").await;
    assert!(html.contains("Sign in"), "{html}");
    assert!(!html.contains("Proxy address"), "{html}");

    // With the default password still in place: change it first.
    b.call("login", &[("password", "admin")]).await;
    assert_eq!(b.call("settings", &[]).await.status(), 403);
}

#[tokio::test]
async fn shows_every_value_with_its_origin() {
    let dir = std::env::temp_dir().join(format!("strata-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut base = Config::default();
    base.apply("STRATA_URL", "http://pc:8080").unwrap();
    base.apply("PC_AGENT_TOKEN", "very-secret").unwrap();
    base.data_dir = Some(dir.clone());
    let state = AppState::with_settings(Settings::new(base, &["STRATA_URL", "PC_AGENT_TOKEN"]));
    state.settings.set("IDLE_TIMEOUT_SECS", "600").unwrap();
    let url = format!("http://{}", serve(api_router(state)).await);
    let mut b = logged_in(&url).await;

    let view = settings(&mut b).await;
    assert_eq!(view["persistent"], true);
    assert_eq!(view["configured"], true);
    assert_eq!(view["entries"].as_array().unwrap().len(), FIELDS.len());
    assert!(!view.to_string().contains("DATA_DIR"));
    assert_eq!(entry(&view, "STRATA_URL")["source"], "Env");
    assert_eq!(entry(&view, "STRATA_URL")["value"], "http://pc:8080");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["value"], "600");
    assert_eq!(entry(&view, "WAKE_TIMEOUT_SECS")["source"], "Default");
    assert_eq!(entry(&view, "LISTEN_ADDR")["restart"], true);
    assert_eq!(entry(&view, "POWER_AWAKE_WATTS")["value"], "75");
    assert_eq!(entry(&view, "POWER_ASLEEP_WATTS")["value"], "3");
    assert_eq!(entry(&view, "PRICE_CT_PER_KWH")["value"], "30");
    assert_eq!(entry(&view, "PRICE_CT_PER_KWH")["source"], "Default");
    assert_eq!(entry(&view, "PC_AGENT_TOKEN")["value"], "set");
    assert!(!view.to_string().contains("very-secret"));

    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains("Strata address"), "{html}");
    assert!(html.contains("Environment"), "{html}");
    let html = b.page("/admin/system").await;
    assert!(html.contains("Takes effect after a restart"), "{html}");
    assert!(
        html.contains(&format!("Version {}", env!("CARGO_PKG_VERSION"))),
        "{html}"
    );
    assert!(html.contains(&dir.display().to_string()), "{html}");
    let html = b.page("/admin/pc").await;
    assert!(html.contains("Saved here"), "{html}");
    assert!(!html.contains("very-secret"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn system_explains_a_missing_data_dir() {
    let s = setup(true, |c| c.strata_url = None).await;
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    assert_eq!(view["persistent"], false);
    assert_eq!(view["configured"], false);
    let html = b.page("/admin/system").await;
    assert!(html.contains("DATA_DIR is not set"), "{html}");
}

#[tokio::test]
async fn each_area_shows_its_own_settings() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let areas = [
        (
            "/admin/pc",
            &[
                "PC_MAC",
                "WOL_ADDR",
                "PC_AGENT_URL",
                "PC_AGENT_TOKEN",
                "SHELLY_URL",
                "PLUG_SAMPLE_SECS",
                "IDLE_TIMEOUT_SECS",
                "WAKE_TIMEOUT_SECS",
                "WOL_RESEND_SECS",
                "HEALTH_POLL_MS",
                "IDLE_CHECK_SECS",
                "SLEEP_RETRY_SECS",
                "SLEEP_SETTLE_SECS",
            ][..],
        ),
        (
            "/admin/strata/connection",
            &[
                "STRATA_URL",
                "CODE_BENCH_URL",
                "UPDATE_SUMMARY_TIMEOUT_SECS",
                "CODE_BENCH_TASK_TIMEOUT_SECS",
            ][..],
        ),
        (
            "/admin/system",
            &[
                "LISTEN_ADDR",
                "MAX_BODY_BYTES",
                "PING_INTERVAL_SECS",
                "PROBE_TIMEOUT_MS",
                "POWER_AWAKE_WATTS",
                "POWER_ASLEEP_WATTS",
                "PRICE_CT_PER_KWH",
            ][..],
        ),
    ];
    let all: Vec<&str> = areas
        .iter()
        .flat_map(|(_, keys)| keys.iter().copied())
        .collect();
    // Every setting has its area, so a new one fails here until it is placed.
    let mut fields: Vec<&str> = FIELDS.iter().map(|f| f.key).collect();
    let mut placed = all.clone();
    fields.sort_unstable();
    placed.sort_unstable();
    assert_eq!(placed, fields);
    for (path, keys) in areas {
        let html = b.page(path).await;
        for key in &all {
            let field = format!("id=\"setting-{key}\"");
            assert_eq!(html.contains(&field), keys.contains(key), "{path}: {key}");
        }
    }
    assert!(!b.page("/admin/access").await.contains("id=\"setting-"));
}

#[tokio::test]
async fn reported_mac_is_shown_above_the_saved_one() {
    let s = setup(true, |_| {}).await;
    s.state.settings.set("PC_MAC", "01:02:03:04:05:06").unwrap();
    s.state.settings.set_reported_mac([0xaa; 6]);
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    let mac = entry(&view, "PC_MAC");
    assert_eq!(mac["source"], "Reported");
    assert_eq!(mac["value"], "AA:AA:AA:AA:AA:AA");
    assert_eq!(mac["saved"], "01:02:03:04:05:06");
    let html = b.page("/admin/pc").await;
    assert!(html.contains("Reported by PC"), "{html}");
    assert!(html.contains("only a fallback"), "{html}");
    assert!(html.contains("value=\"01:02:03:04:05:06\""), "{html}");
}

async fn save(b: &mut Browser, key: &str, value: &str) -> reqwest::Response {
    save_settings(b, &[(key, value)]).await
}

#[tokio::test]
async fn a_section_is_saved_in_one_call() {
    use strata_nas_proxy::events::EventKind;
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(
        &mut b,
        &[("IDLE_TIMEOUT_SECS", "600"), ("WAKE_TIMEOUT_SECS", "99")],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let cfg = s.state.cfg();
    assert_eq!(cfg.idle_timeout.as_secs(), 600);
    assert_eq!(cfg.wake_timeout.as_secs(), 99);
    let view = settings(&mut b).await;
    assert_eq!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
    assert_eq!(entry(&view, "WAKE_TIMEOUT_SECS")["source"], "Saved");
    let saved: Vec<_> = s
        .state
        .events
        .recent()
        .into_iter()
        .filter(|e| e.kind == EventKind::SettingSaved)
        .filter_map(|e| e.detail)
        .collect();
    assert_eq!(saved.len(), 2, "{saved:?}");
}

#[tokio::test]
async fn one_invalid_value_saves_none_and_names_its_key() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(&mut b, &[("IDLE_TIMEOUT_SECS", "600"), ("PC_MAC", "12:34")]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("PC_MAC: "));
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
    let view = settings(&mut b).await;
    assert_ne!(entry(&view, "IDLE_TIMEOUT_SECS")["source"], "Saved");
}

#[tokio::test]
async fn an_empty_secret_stays_as_it_is() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save(&mut b, "PC_AGENT_TOKEN", "very-secret").await.status(),
        200
    );
    let resp = save_settings(
        &mut b,
        &[("PC_AGENT_TOKEN", ""), ("IDLE_TIMEOUT_SECS", "5")],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let cfg = s.state.cfg();
    assert_eq!(cfg.pc_agent_token.as_deref(), Some("very-secret"));
    assert_eq!(cfg.idle_timeout.as_secs(), 5);

    // Only a reset removes it.
    b.call("reset-setting", &[("key", "PC_AGENT_TOKEN")]).await;
    assert_ne!(s.state.cfg().pc_agent_token.as_deref(), Some("very-secret"));
}

#[tokio::test]
async fn saving_and_resetting_need_a_login() {
    let s = setup(true, |_| {}).await;
    let mut b = Browser::new(&s.url);
    assert_eq!(save(&mut b, "IDLE_TIMEOUT_SECS", "1").await.status(), 401);
    assert_eq!(
        b.call("reset-setting", &[("key", "IDLE_TIMEOUT_SECS")])
            .await
            .status(),
        401
    );
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
}

#[tokio::test]
async fn saved_strata_url_applies_at_once_and_reset_restores_it() {
    use std::sync::{Arc, atomic::Ordering};
    let s = setup(true, |_| {}).await;
    let other = Arc::new(FakePc::default());
    other.awake.store(true, Ordering::SeqCst);
    let other_strata = serve_strata(other.clone()).await;
    let mut b = logged_in(&s.url).await;

    let resp = save(&mut b, "STRATA_URL", &format!("http://{other_strata}")).await;
    assert_eq!(resp.status(), 200);
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(other.api_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        entry(&settings(&mut b).await, "STRATA_URL")["source"],
        "Saved"
    );

    let resp = b.call("reset-setting", &[("key", "STRATA_URL")]).await;
    assert_eq!(resp.status(), 200);
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 1);
    assert_ne!(
        entry(&settings(&mut b).await, "STRATA_URL")["source"],
        "Saved"
    );
}

#[tokio::test]
async fn power_and_price_are_saved_with_decimals_and_apply_at_once() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let resp = save_settings(
        &mut b,
        &[
            ("POWER_AWAKE_WATTS", "110"),
            ("POWER_ASLEEP_WATTS", "1.5"),
            ("PRICE_CT_PER_KWH", "32.5"),
        ],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let cfg = s.state.cfg();
    assert_eq!(cfg.power_awake_watts, 110.0);
    assert_eq!(cfg.power_asleep_watts, 1.5);
    assert_eq!(cfg.price_ct_per_kwh, 32.5);
    let view = settings(&mut b).await;
    assert_eq!(entry(&view, "PRICE_CT_PER_KWH")["source"], "Saved");
    assert_eq!(entry(&view, "PRICE_CT_PER_KWH")["value"], "32.5");
    let html = b.page("/admin/system").await;
    assert!(html.contains("ct/kWh"), "{html}");
    assert!(html.contains("value=\"32.5\""), "{html}");
}

#[tokio::test]
async fn saved_idle_timeout_brings_sleep_forward() {
    use std::{sync::atomic::Ordering, time::Duration};
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    post_json(&s, "/v1/messages", serde_json::json!({})).await;
    assert_eq!(save(&mut b, "IDLE_TIMEOUT_SECS", "1").await.status(), 200);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_values_are_rejected_with_a_message() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    for (key, value) in [
        ("IDLE_TIMEOUT_SECS", "bald"),
        ("PC_MAC", "12:34"),
        ("STRATA_URL", "pc:8080"),
        ("WOL_ADDR", "192.168.1.255"),
        ("IDLE_TIMEOUT_SECS", ""),
        ("DATA_DIR", "/tmp"),
        ("POWER_AWAKE_WATTS", "0"),
        ("POWER_ASLEEP_WATTS", "-1"),
        ("PRICE_CT_PER_KWH", "billig"),
    ] {
        let resp = save(&mut b, key, value).await;
        assert_eq!(resp.status(), 400, "{key}={value}");
        assert!(!resp.text().await.unwrap().is_empty());
    }
    let resp = save(&mut b, "IDLE_TIMEOUT_SECS", "bald").await;
    assert!(resp.text().await.unwrap().contains("whole number"));
    assert_eq!(s.state.cfg().idle_timeout.as_secs(), 3600);
    assert!(
        s.state
            .settings
            .entries()
            .iter()
            .all(|e| e.source != strata_nas_proxy::settings::Source::Saved)
    );
}

#[tokio::test]
async fn changes_are_logged_without_secrets() {
    use strata_nas_proxy::events::EventKind;
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save(&mut b, "PC_AGENT_TOKEN", "very-secret").await.status(),
        200
    );
    b.call("reset-setting", &[("key", "PC_AGENT_TOKEN")]).await;
    let events = s.state.events.recent();
    let changes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::SettingSaved | EventKind::SettingReset))
        .collect();
    assert_eq!(changes.len(), 2);
    assert!(
        changes
            .iter()
            .all(|e| e.detail.as_deref() == Some("PC_AGENT_TOKEN"))
    );
    let all = serde_json::to_string(&events).unwrap();
    assert!(!all.contains("very-secret"));
}

#[tokio::test]
async fn saved_values_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("strata-page-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = Config {
        data_dir: Some(dir.clone()),
        ..Config::default()
    };
    let first = AppState::new(base.clone());
    let mut b = logged_in(&format!("http://{}", serve(api_router(first)).await)).await;
    assert_eq!(save(&mut b, "WAKE_TIMEOUT_SECS", "99").await.status(), 200);
    let second = AppState::new(base);
    assert_eq!(second.cfg().wake_timeout.as_secs(), 99);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn timeouts_suggest_assumed_speeds_without_benchmarks() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    let view = settings(&mut b).await;
    for (key, secs) in [
        ("UPDATE_SUMMARY_TIMEOUT_SECS", 180),
        ("CODE_BENCH_TASK_TIMEOUT_SECS", 3360),
    ] {
        let e = entry(&view, key);
        assert_eq!(e["optional"], true, "{e}");
        assert!(e["value"].is_null(), "{e}");
        assert_eq!(e["suggestion"]["secs"], secs, "{e}");
        let from = e["suggestion"]["from"].as_str().unwrap();
        assert!(from.contains("assumed 30 tok/s"), "{from}");
        assert!(from.contains("Compare models"), "{from}");
    }
    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains("Timeouts"), "{html}");
    for key in [
        "UPDATE_SUMMARY_TIMEOUT_SECS",
        "CODE_BENCH_TASK_TIMEOUT_SECS",
    ] {
        assert!(html.contains(&format!("id=\"setting-{key}\"")), "{html}");
    }
    assert!(html.contains("placeholder=\"3360\""), "{html}");
}

/// Speeds of three installed models and one deleted since; one installed
/// model's benchmark has no decode speed, and the slowest lacks its
/// prompt speed.
fn with_benchmarks(s: &Setup) {
    let mut report = strata_info(true);
    report
        .info
        .models
        .push(model("coder-IQ1_M", Some("coder"), 1, false));
    report
        .info
        .models
        .push(model("qwen-Q2", Some("qwen"), 1, false));
    let bench = |id: &str, decode: Option<f64>, prompt: Option<f64>, context: u32| {
        let mut b = benchmark(id, "");
        b.decode_tps = decode;
        b.prompt_tps = prompt;
        b.context = Some(context);
        b
    };
    report.info.benchmarks = vec![
        bench("IQ3_XXS", Some(40.0), Some(400.0), 32768),
        bench("coder-IQ1_M", Some(12.0), None, 262144),
        bench("qwen-Q2", None, Some(100.0), 65536),
        bench("deleted-Q1", Some(5.0), Some(50.0), 65536),
    ];
    *s.pc.strata_info.lock().unwrap() = Some(report);
}

#[tokio::test]
async fn timeouts_follow_the_slowest_installed_model() {
    let s = setup(true, |_| {}).await;
    with_benchmarks(&s);
    let mut b = logged_in(&s.url).await;
    assert_eq!(b.call("strata", &[]).await.status(), 200);
    let view = settings(&mut b).await;
    // (262144 / 2 / 300 + 2000 / 12) × 2 = 1207 s; 50 000 / 12 × 2 = 8333 s.
    // 21 min holds only for prompt speeds of about 283 to 302 tok/s, so the
    // assumed 300 applies, not 400 or 100 from the other benchmarks.
    for (key, secs) in [
        ("UPDATE_SUMMARY_TIMEOUT_SECS", 1260),
        ("CODE_BENCH_TASK_TIMEOUT_SECS", 8340),
    ] {
        let suggestion = &entry(&view, key)["suggestion"];
        assert_eq!(suggestion["secs"], secs, "{key}: {suggestion}");
        assert_eq!(
            suggestion["from"], "slowest model coder-IQ1_M at 12 tok/s",
            "{key}"
        );
    }
    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains("placeholder=\"1260\""), "{html}");
    assert!(
        html.contains("slowest model coder-IQ1_M at 12 tok/s"),
        "{html}"
    );
}

#[tokio::test]
async fn a_saved_timeout_overrides_the_suggestion_until_reset() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        save(&mut b, "UPDATE_SUMMARY_TIMEOUT_SECS", "45")
            .await
            .status(),
        200
    );
    let view = settings(&mut b).await;
    let e = entry(&view, "UPDATE_SUMMARY_TIMEOUT_SECS");
    assert_eq!(e["source"], "Saved");
    assert_eq!(e["value"], "45");
    assert_eq!(
        strata_nas_proxy::timeouts::update_summary(&s.state).as_secs(),
        45
    );
    let used_instead = "Suggested: 180 s (3 min), from assumed 30 tok/s, run Compare models for a better value. Your value is used instead.";
    let html = b.page("/admin/strata/connection").await;
    assert!(html.contains(used_instead), "{html}");
    b.call("reset-setting", &[("key", "UPDATE_SUMMARY_TIMEOUT_SECS")])
        .await;
    let view = settings(&mut b).await;
    let e = entry(&view, "UPDATE_SUMMARY_TIMEOUT_SECS");
    assert!(e["value"].is_null(), "{e}");
    let html = b.page("/admin/strata/connection").await;
    assert!(!html.contains("Your value is used instead."), "{html}");
    assert_eq!(e["suggestion"]["secs"], 180, "{e}");
    assert_eq!(
        strata_nas_proxy::timeouts::update_summary(&s.state).as_secs(),
        180
    );
}

#[tokio::test]
async fn a_timeout_under_ten_seconds_is_refused() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    for value in ["9", "0", "lang", "1.5"] {
        let resp = save(&mut b, "CODE_BENCH_TASK_TIMEOUT_SECS", value).await;
        assert_eq!(resp.status(), 400, "{value}");
        assert!(
            resp.text()
                .await
                .unwrap()
                .contains("CODE_BENCH_TASK_TIMEOUT_SECS: "),
            "{value}"
        );
    }
    assert_eq!(
        save(&mut b, "CODE_BENCH_TASK_TIMEOUT_SECS", "10")
            .await
            .status(),
        200
    );
    assert_eq!(s.state.cfg().code_bench_task_timeout.unwrap().as_secs(), 10);
}
