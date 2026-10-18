//! The Strata page: what the PC service reports about Strata, and jobs
//! started from the page, against the simulated PC and its service.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
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

fn strata_key(s: &Setup) -> Option<String> {
    s.state.cfg().strata_api_key.clone()
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
async fn install_wakes_the_pc_and_hands_it_a_new_strata_key() {
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
    let key = body["api_key"].as_str().unwrap().to_string();
    assert_eq!(key.len(), 64);
    let kinds = kinds(&s.state);
    assert_eq!(
        kinds.last(),
        Some(&EventKind::StrataJobStarted),
        "{kinds:?}"
    );
    assert!(kinds.contains(&EventKind::WakeRequested), "{kinds:?}");
    // Until Strata is set up with it, the key counts nowhere.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(strata_key(&s), None);

    s.pc.update_job(json!({"state": "done"}));
    eventually("Key gilt", || strata_key(&s).as_deref() == Some(&key)).await;
    // The key reaches Strata with requests, never the page.
    assert!(!text.contains(&key));
    assert!(!page(&mut b).await.to_string().contains(&key));
    assert!(
        !b.call("settings", &[])
            .await
            .text()
            .await
            .unwrap()
            .contains(&key)
    );
    post_json(&s, "/v1/chat/completions", json!({})).await;
    let seen = s.pc.api_auth.lock().unwrap().clone().unwrap();
    assert_eq!(seen.authorization, Some(format!("Bearer {key}")));
}

#[tokio::test]
async fn a_failed_install_leaves_the_strata_key_alone() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    s.pc.update_job(json!({"state": "failed", "error": "kein Platz"}));
    eventually("Fehlschlag vermerkt", || {
        kinds(&s.state).contains(&EventKind::StrataJobFailed)
    })
    .await;
    assert_eq!(strata_key(&s), None);
}

#[tokio::test]
async fn a_new_key_still_counts_after_a_restart_of_the_proxy() {
    let dir = std::env::temp_dir().join(format!("strata-pending-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.clone();
    let s = setup(true, move |c| c.data_dir = Some(data)).await;
    let mut b = logged_in(&s.url).await;
    b.call("strata-job", &[("kind", "set_key")]).await;
    let key = last_body(&s)["api_key"].as_str().unwrap().to_string();

    let restarted = strata_nas_proxy::AppState::new((*s.state.cfg()).clone());
    s.pc.update_job(json!({"state": "done"}));
    restarted.pc_status(&restarted.agent()).await.unwrap();
    assert_eq!(
        restarted.cfg().strata_api_key.as_deref(),
        Some(key.as_str())
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_status_from_before_the_start_does_not_end_the_job() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    b.call("strata-job", &[("kind", "set_key")]).await;
    let key = last_body(&s)["api_key"].as_str().unwrap().to_string();
    // Like a status answered just before the job began.
    let stale = strata_nas_proxy::agent::PcStatus::default();
    strata_nas_proxy::strata::note_status(&s.state, &s.state.agent(), &stale).await;
    s.pc.update_job(json!({"state": "done"}));
    eventually("neuer Key gilt", || strata_key(&s).as_deref() == Some(&key)).await;
}

#[tokio::test]
async fn a_refusal_comes_with_its_reason() {
    let s = setup(true, |_| {}).await;
    *s.pc.refuse_job.lock().unwrap() = Some("Nicht genug Platz auf C:".into());
    let mut b = logged_in(&s.url).await;
    let resp = install(&mut b).await;
    assert_eq!(resp.status(), 400);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("Nicht genug Platz auf C:")
    );
    assert_eq!(strata_key(&s), None);
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
    live.until("Vorgang", |v| v["strata_job"]["kind"] == "install")
        .await;
    s.pc.update_job(json!({"step": "4/7: Modell laden", "percent": 40}));
    live.until("40 %", |v| v["strata_job"]["percent"] == 40)
        .await;
    s.pc.update_job(json!({"state": "done"}));
    live.until("Ende", |v| v["strata_job"].is_null()).await;
    let kinds = kinds(&s.state);
    assert!(kinds.contains(&EventKind::StrataJobDone), "{kinds:?}");
}

#[tokio::test]
async fn without_a_page_nobody_asks_for_progress() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.job_polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failed_job_is_recorded_with_why() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    s.pc.update_job(json!({"state": "failed", "error": "[X] Download abgebrochen"}));
    eventually("Fehlschlag vermerkt", || {
        kinds(&s.state).contains(&EventKind::StrataJobFailed)
    })
    .await;
    let event = s.state.events.recent().pop().unwrap();
    assert!(
        event.detail.unwrap().contains("[X] Download abgebrochen"),
        "Grund fehlt"
    );
}

#[tokio::test]
async fn cancel_ends_the_job() {
    let s = setup(true, |_| {}).await;
    let mut b = logged_in(&s.url).await;
    assert_eq!(install(&mut b).await.status(), 200);
    assert_eq!(b.call("strata-cancel", &[]).await.status(), 200);
    assert_eq!(s.pc.job_cancels.load(Ordering::SeqCst), 1);
    eventually("Abbruch vermerkt", || {
        kinds(&s.state).contains(&EventKind::StrataJobCancelled)
    })
    .await;
    let resp = b.call("strata-cancel", &[]).await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("Es läuft kein Vorgang"));
}

#[tokio::test]
async fn a_new_strata_key_counts_once_strata_took_it_over() {
    let s = setup(true, |_| {}).await;
    s.state.settings.set("STRATA_API_KEY", "alt").unwrap();
    let mut b = logged_in(&s.url).await;
    assert_eq!(
        b.call("strata-job", &[("kind", "set_key")]).await.status(),
        200
    );
    let key = last_body(&s)["api_key"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(strata_key(&s).as_deref(), Some("alt"));

    s.pc.update_job(json!({"state": "done"}));
    eventually("neuer Key gilt", || strata_key(&s).as_deref() == Some(&key)).await;
    assert!(kinds(&s.state).contains(&EventKind::StrataKeyChanged));
}

#[tokio::test]
async fn a_failed_key_change_keeps_the_old_key() {
    let s = setup(true, |_| {}).await;
    s.state.settings.set("STRATA_API_KEY", "alt").unwrap();
    let mut b = logged_in(&s.url).await;
    b.call("strata-job", &[("kind", "set_key")]).await;
    s.pc.update_job(json!({"state": "failed", "error": "Strata startet nicht"}));
    eventually("Fehlschlag vermerkt", || {
        kinds(&s.state).contains(&EventKind::StrataJobFailed)
    })
    .await;
    assert_eq!(strata_key(&s).as_deref(), Some("alt"));
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
    let mut info = strata_info(true);
    info["update_available"] = json!(true);
    info["benchmark"] =
        json!({"model": "IQ3_XXS", "result": "38.5 Token/s", "at": 1_760_000_000u64});
    *s.pc.strata_info.lock().unwrap() = Some(info);
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
    assert!(html.contains("38.5 Token/s with IQ3_XXS"), "{html}");
    assert!(html.contains("Update available"), "{html}");
}

#[tokio::test]
async fn another_model_gets_the_strata_key_in_use() {
    let s = setup(true, |_| {}).await;
    s.state
        .settings
        .set("STRATA_API_KEY", "strata-geheim-0123456789")
        .unwrap();
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
    assert_eq!(last_body(&s)["api_key"], "strata-geheim-0123456789");
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
        ("/admin/strata/connection", "Create new Strata key"),
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
    assert!(html.contains("id=\"setting-STRATA_API_KEY\""), "{html}");
    let fields = html.matches("id=\"setting-").count();
    assert_eq!(fields, 2, "{html}");
    let models = b.page("/admin/strata").await;
    assert!(!models.contains("id=\"setting-"), "{models}");
}

#[tokio::test]
async fn a_running_job_shows_and_locks_the_buttons_in_every_tab() {
    let s = setup(true, |_| {}).await;
    let mut info = strata_info(true);
    info["models"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "IQ2_M", "disk_bytes": 60_000_000_000u64, "active": false}));
    *s.pc.strata_info.lock().unwrap() = Some(info);
    *s.pc.strata_job.lock().unwrap() =
        Some(json!({"kind": "update", "state": "running", "percent": 40}));
    let b = logged_in(&s.url).await;
    for (path, button) in [
        ("/admin/strata", "Make active"),
        ("/admin/strata/version", "Run benchmark"),
        ("/admin/strata/connection", "Create new Strata key"),
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
fn with_two_models(projection: Option<bool>) -> Value {
    let mut info = strata_info(true);
    info["families"] = json!([
        {"id": "qwen", "title": "Qwen3.8-Flash-Next", "speed_projection": true},
        {"id": "swift", "title": "Swift 1.5", "speed_projection": false},
    ]);
    let mut qwen = json!({"id": "IQ3_XXS", "family": "qwen", "size": "IQ3_XXS",
        "disk_bytes": 81_000_000_000u64, "active": true});
    let mut swift = json!({"id": "swift-IQ2_XS", "family": "swift", "size": "IQ2_XS",
        "disk_bytes": 40_000_000_000u64, "active": false});
    if let Some(on) = projection {
        qwen["speed_projection"] = json!(on);
        swift["speed_projection"] = json!(false);
    }
    info["models"] = json!([qwen, swift]);
    info
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
    let mut info = with_two_models(None);
    info["models"][0]["context"] = json!(65536);
    info["models"][1]["context"] = json!(524288);
    *s.pc.strata_info.lock().unwrap() = Some(info);
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
