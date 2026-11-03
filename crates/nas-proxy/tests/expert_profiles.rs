//! Learned expert profiles from the proxy's side: the web UI's profile jobs,
//! the tokens and minutes a learning profile is counted, the end of learning
//! at a limit, and what the Strata page, run settings and the optimization
//! prompt say about the profile.

use crate::common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use strata_admin_ui::api::ExpertCounts;
use strata_nas_proxy::events::EventKind;

const CHAT: &str = "/v1/chat/completions";

/// A last chunk as Strata's `openai_chunks` writes it, with `usage`.
fn stream_with(usage: &str) -> Vec<String> {
    vec![
        format!(
            r#"data: {{"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1761426000, "model": "Qwen3.6-27B", "choices": [{{"index": 0, "delta": {{"content": "Hi"}}, "finish_reason": "stop"}}], "usage": {usage}}}"#
        ) + "\n\n",
        "data: [DONE]\n\n".into(),
    ]
}

/// 1000 prompt tokens Strata read, 50 output tokens.
const READ_ALL: &str = r#"{"prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050}"#;
/// 5200 prompt tokens of which 4800 cached, 41 output tokens.
const CACHED: &str = r#"{"prompt_tokens": 5200, "completion_tokens": 41, "total_tokens": 5241, "prompt_tokens_details": {"cached_tokens": 4800}}"#;

async fn setup_with_model() -> Setup {
    let s = setup(true, |_| {}).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    s
}

/// One check of the PC service's status, as the idle loop makes.
async fn status_check(s: &Setup) {
    s.state.ask_pc(&s.state.agent()).await;
}

async fn profile_job(b: &mut Browser, fields: &[(&str, &str)]) -> reqwest::Response {
    let mut all = vec![("kind", "expert_profile"), ("model", "IQ3_XXS")];
    all.extend_from_slice(fields);
    b.call("strata-job", &all).await
}

async fn learn_new(b: &mut Browser, name: &str, tokens: &str, minutes: &str) -> reqwest::Response {
    profile_job(
        b,
        &[
            ("profile", name),
            ("learn", "true"),
            ("new_profile", "true"),
            ("tokens", tokens),
            ("minutes", minutes),
        ],
    )
    .await
}

/// Starts learning `coding` with the limits and lets the PC service's job
/// end, which reports it learning from then on.
async fn learning(s: &Setup, b: &mut Browser, tokens: &str, minutes: &str) {
    assert_eq!(learn_new(b, "coding", tokens, minutes).await.status(), 200);
    job_done(s).await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 0);
    status_check(s).await;
}

/// The PC service's job ends; the proxy notes it at its next check.
async fn job_done(s: &Setup) {
    s.pc.update_job(|j| j.state = StrataJobState::Done);
    status_check(s).await;
}

async fn answered_request(s: &Setup, usage: &str) {
    s.pc.set_strata_stream(CHAT, stream_with(usage), Duration::from_millis(5));
    let resp = post_json(
        s,
        CHAT,
        json!({"model": "any", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
}

fn counts(s: &Setup) -> Option<ExpertCounts> {
    s.state
        .expert_learning
        .counts()
        .into_iter()
        .find(|c| c.model == "IQ3_XXS" && c.profile == "coding")
}

async fn tokens_counted(s: &Setup, tokens: u64) {
    eventually(&format!("{tokens} tokens counted"), || {
        counts(s).is_some_and(|c| c.tokens == tokens)
    })
    .await;
}

fn bodies(s: &Setup) -> Vec<Value> {
    s.pc.job_bodies.lock().unwrap().clone()
}

/// The event of a learned profile, asking for the status until it comes: a
/// status the proxy's own poll fetched before the job ended can arrive late
/// and still say the job runs.
async fn learned_event(s: &Setup) -> strata_nas_proxy::events::Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        status_check(s).await;
        let found = s
            .state
            .events
            .recent()
            .into_iter()
            .find(|e| e.kind == EventKind::ExpertProfileLearned);
        if let Some(event) = found {
            return event;
        }
        assert!(std::time::Instant::now() < deadline, "no learned event");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn learning_ended(s: &Setup) -> bool {
    bodies(s).contains(&json!({"kind": "expert_profile", "model": "IQ3_XXS", "profile": "coding"}))
}

fn coding_profiles(learning: bool) -> ExpertProfiles {
    ExpertProfiles {
        active: Some("coding".into()),
        learning,
        held: false,
        by_hand: false,
        profiles: vec![ExpertProfile {
            name: "coding".into(),
            bytes: Some(1024),
            saved: Some(1_760_000_000),
        }],
    }
}

// --- the web UI's jobs ---

#[tokio::test]
async fn learn_new_profile_sends_the_expert_profile_job() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        learn_new(&mut b, "coding", "1000000", "").await.status(),
        200
    );
    assert_eq!(
        bodies(&s),
        [json!({"kind": "expert_profile", "model": "IQ3_XXS", "profile": "coding", "learn": true})]
    );
}

async fn refused_before_the_pc_service(name: &str, tokens: &str) -> String {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    let resp = learn_new(&mut b, name, tokens, "").await;
    assert_eq!(resp.status(), 400, "{name}");
    assert!(bodies(&s).is_empty(), "{name}");
    resp.text().await.unwrap()
}

#[tokio::test]
async fn empty_profile_name_is_refused_before_the_pc_service() {
    let why = refused_before_the_pc_service(" ", "1000").await;
    assert!(why.contains("name"), "{why}");
}

#[tokio::test]
async fn profile_name_over_40_characters_is_refused_before_the_pc_service() {
    let why = refused_before_the_pc_service(&"a".repeat(41), "1000").await;
    assert!(why.contains("40"), "{why}");
}

#[tokio::test]
async fn profile_name_with_other_characters_is_refused_before_the_pc_service() {
    let why = refused_before_the_pc_service("code/review", "1000").await;
    assert!(why.contains("letters"), "{why}");
}

#[tokio::test]
async fn profile_named_standard_is_refused_before_the_pc_service() {
    let why = refused_before_the_pc_service("Standard", "1000").await;
    assert!(why.contains("Standard"), "{why}");
}

#[tokio::test]
async fn limit_below_1_is_refused_before_the_pc_service() {
    let why = refused_before_the_pc_service("coding", "0").await;
    assert!(why.contains("token limit"), "{why}");
}

#[tokio::test]
async fn learn_new_profile_with_a_taken_name_is_refused() {
    let s = setup_with_model().await;
    s.pc.set_expert_profiles("IQ3_XXS", coding_profiles(false));
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    let resp = learn_new(&mut b, "coding", "1000", "").await;
    assert_eq!(resp.status(), 409);
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("coding: IQ3_XXS has an expert profile of that name already."),
        "refusal: {text}"
    );
    assert!(bodies(&s).is_empty());
}

// --- counting ---

#[tokio::test]
async fn requests_while_learning_count_read_and_output_tokens() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
}

#[tokio::test]
async fn cached_prompt_tokens_do_not_count() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    answered_request(&s, CACHED).await;
    tokens_counted(&s, 400 + 41).await;
}

#[tokio::test]
async fn requests_while_learning_is_held_do_not_count() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", true, 0);
    status_check(&s).await;
    answered_request(&s, READ_ALL).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(counts(&s).unwrap().tokens, 0);
}

// --- the end of learning ---

#[tokio::test]
async fn token_limit_reached_ends_learning() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000", "").await;
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
    status_check(&s).await;
    assert!(learning_ended(&s), "{:?}", bodies(&s));
}

#[tokio::test]
async fn minute_limit_reached_ends_learning() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "", "2").await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 119);
    status_check(&s).await;
    assert!(!learning_ended(&s), "{:?}", bodies(&s));
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 120);
    status_check(&s).await;
    assert!(learning_ended(&s), "{:?}", bodies(&s));
}

#[tokio::test]
async fn learning_without_limits_never_ends_by_itself() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "", "").await;
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 1_000_000);
    status_check(&s).await;
    assert!(!learning_ended(&s), "{:?}", bodies(&s));
}

#[tokio::test]
async fn learned_profile_records_an_event_with_tokens_and_time() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000", "").await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 7_800);
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
    status_check(&s).await;
    assert!(learning_ended(&s));
    s.pc.clear_expert_learning();
    job_done(&s).await;
    let event = learned_event(&s).await;
    assert_eq!(
        event.detail.as_deref(),
        Some("Expert profile coding of IQ3_XXS learned: 1K tokens, 2 h 10 min")
    );
    let c = counts(&s).unwrap();
    assert!(!c.learning);
    assert_eq!((c.total_tokens, c.total_secs), (1050, 7_800));
}

#[tokio::test]
async fn learn_more_counts_the_round_from_zero_and_adds_to_the_totals() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
    // Stop learning, then Learn more.
    assert_eq!(
        profile_job(&mut b, &[("profile", "coding")]).await.status(),
        200
    );
    s.pc.clear_expert_learning();
    job_done(&s).await;
    let more = profile_job(
        &mut b,
        &[("profile", "coding"), ("learn", "true"), ("tokens", "500")],
    )
    .await;
    assert_eq!(more.status(), 200);
    let c = counts(&s).unwrap();
    assert!(c.learning);
    assert_eq!((c.tokens, c.token_limit), (0, Some(500)));
    assert_eq!(c.total_tokens, 1050);
}

#[tokio::test]
async fn learning_counts_survive_a_proxy_restart() {
    let dir = data_dir("expert-counts-restart");
    let s = setup(true, |c| c.data_dir = Some(dir.clone())).await;
    *s.pc.strata_info.lock().unwrap() = Some(strata_info(true));
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    answered_request(&s, READ_ALL).await;
    tokens_counted(&s, 1050).await;
    let again = restarted(&s);
    let c = again
        .expert_learning
        .counts()
        .into_iter()
        .find(|c| c.profile == "coding")
        .unwrap();
    assert_eq!((c.tokens, c.token_limit), (1050, Some(1_000_000)));
}

// --- what the proxy shows ---

#[tokio::test]
async fn run_settings_name_the_expert_profile() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].expert_profiles = Some(coding_profiles(false));
    // A run in the first report came before the history: no settings.
    *s.pc.strata_info.lock().unwrap() = Some(report.clone());
    let mut b = logged_in(&s.url).await;
    b.call("strata", &[]).await;
    report.info.benchmarks = vec![StrataBenchmark {
        at: 1_760_000_600,
        decode_tps: Some(43.0),
        ..benchmark("IQ3_XXS", "43 Token/s")
    }];
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let v: Value = b.call("strata", &[]).await.json().await.unwrap();
    let settings = &v["benchmarks"][0]["runs"][0]["settings"];
    assert_eq!(settings["expert_profile"], "coding", "{v}");
}

#[tokio::test]
async fn optimization_prompt_names_the_expert_profile() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].expert_profiles = Some(coding_profiles(false));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let mut b = logged_in(&s.url).await;
    let resp = b
        .call("strata-optimization-prompt", &[("model", "IQ3_XXS")])
        .await;
    let text: String = resp.json().await.unwrap();
    assert!(text.contains("- Expert profile: coding"), "{text}");
}

#[tokio::test]
async fn strata_page_shows_learning_progress_on_the_model_card() {
    let s = setup_with_model().await;
    let mut b = logged_in(&s.url).await;
    learning(&s, &mut b, "1000000", "").await;
    s.pc.set_expert_learning("IQ3_XXS", "coding", false, 7_800);
    status_check(&s).await;
    let mut report = strata_info(true);
    report.info.models[0].expert_profiles = Some(coding_profiles(true));
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let html = b.page("/admin/strata").await;
    assert!(
        html.contains("Expert profile: coding, learning: 0 of 1.0M tokens · 2 h 10 min"),
        "{html}"
    );
}

#[tokio::test]
async fn strata_page_shows_set_by_hand_without_buttons() {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    report.info.models[0].env = Some(Default::default());
    report.info.models[0].expert_profiles = Some(ExpertProfiles {
        by_hand: true,
        ..ExpertProfiles::default()
    });
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    let html = b.page("/admin/strata").await;
    assert!(html.contains("Expert profile set by hand"), "{html}");
    assert!(!html.contains("Learn new profile"), "{html}");
}

const LEARNED_WARNING: &str = "With this Strata, a learned profile together with --adapt-async or \
     --expert-cache-per-layer can make the model answer wrongly on long prompts. If answers turn \
     into nonsense, use Standard or remove these options.";

/// The Strata page for a model with a learned expert profile (set by hand
/// with `by_hand`) that starts with `args` under the active preset's
/// `preset` settings.
async fn strata_page_with(args: &[&str], preset: &[&str], by_hand: bool) -> String {
    let s = setup(true, |_| {}).await;
    let mut report = strata_info(true);
    let model = &mut report.info.models[0];
    model.env = Some(Default::default());
    model.started_args = Some(args.iter().map(|a| a.to_string()).collect());
    model.expert_profiles = Some(if by_hand {
        ExpertProfiles {
            by_hand: true,
            ..ExpertProfiles::default()
        }
    } else {
        coding_profiles(false)
    });
    if !preset.is_empty() {
        model.presets = Some(StrataPresets {
            latest: Some(StrataPreset {
                id: "3f2a9c0e5b7d41c8a6e2f1b0d9c8e7a6".into(),
                name: "IQ3_XXS 64K – Full speed".into(),
                active: true,
                state: None,
                reasons: Vec::new(),
                gain_pct: Some(12.4),
                settings: preset.iter().map(|s| s.to_string()).collect(),
                decode_tps: Some(35.1),
                base_tps: Some(31.2),
            }),
            other_context: false,
        });
    }
    *s.pc.strata_info.lock().unwrap() = Some(report);
    let b = logged_in(&s.url).await;
    b.page("/admin/strata").await
}

fn warns(html: &str) -> bool {
    html.contains(&format!("<p class=\"warning\">{LEARNED_WARNING}</p>"))
}

#[tokio::test]
async fn expert_profiles_warn_when_the_model_starts_with_adapt_async() {
    let html = strata_page_with(&["--native", "a.gguf", "--adapt-async", "1"], &[], false).await;
    assert!(warns(&html), "{html}");
}

#[tokio::test]
async fn expert_profiles_warn_when_the_model_starts_with_expert_cache_per_layer() {
    let html = strata_page_with(
        &["--native", "a.gguf", "--expert-cache-per-layer"],
        &[],
        false,
    )
    .await;
    assert!(warns(&html), "{html}");
}

#[tokio::test]
async fn expert_profiles_warn_when_the_active_preset_sets_adapt_async() {
    let html = strata_page_with(&["--native", "a.gguf"], &["--adapt-async 1"], false).await;
    assert!(warns(&html), "{html}");
}

#[tokio::test]
async fn expert_profiles_show_no_warning_with_adapt_async_0() {
    let html = strata_page_with(&["--native", "a.gguf", "--adapt-async", "0"], &[], false).await;
    assert!(html.contains("Learn new profile"), "{html}");
    assert!(!html.contains(LEARNED_WARNING), "{html}");
}

#[tokio::test]
async fn expert_profiles_show_no_warning_without_either_option() {
    let html = strata_page_with(&["--native", "a.gguf", "--spec", "4"], &[], false).await;
    assert!(html.contains("Learn new profile"), "{html}");
    assert!(!html.contains(LEARNED_WARNING), "{html}");
}

#[tokio::test]
async fn expert_profiles_set_by_hand_show_no_warning() {
    let html = strata_page_with(&["--native", "a.gguf", "--adapt-async", "1"], &[], true).await;
    assert!(html.contains("Expert profile set by hand"), "{html}");
    assert!(!html.contains(LEARNED_WARNING), "{html}");
}
