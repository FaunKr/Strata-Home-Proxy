//! The load lines: each model's Strata output from a start by the service
//! until `/health` answers, kept on the PC and reported in `GET /strata`.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, strata};

/// Strata with IQ3_XXS and swift-IQ2_XS, as setup writes them.
fn installed(s: &Setup) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let setup = SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    for id in ["iq3_xxs", "swift-iq2_xs"] {
        setup.model(
            id,
            json!({"exe": "C:/Strata/engine/strata.exe", "args": ["--native", "a.gguf"],
                "port": 8080, "api_key": "key-0123456789abcdef"}),
        );
    }
    setup
}

/// Starts `id` through its start script, which prints `lines` and then
/// keeps running, or ends if `ends`; Strata answers `/health` only once
/// `answer` is called.
async fn start(s: &Setup, setup: &SetupLeaves, id: &str, lines: &[String], ends: bool) {
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    s.fake.strata_up.store(false, Ordering::SeqCst);
    let script = setup.installation().run_script(id);
    s.fake.command(
        script.file_name().unwrap().to_str().unwrap(),
        FakeCommand {
            lines: lines.to_vec(),
            hang: !ends,
            ..Default::default()
        },
    );
    s.agent
        .set_managed(Managed {
            strata_start: Some(script),
            strata_args: vec![],
            strata_processes: Some(vec![]),
        })
        .unwrap();
    post(s, "/gaming", json!({"enabled": true})).await;
    post(s, "/gaming", json!({"enabled": false})).await;
    let last = format!("{}\n", lines.last().unwrap());
    let log = s.dir.join("strata.log");
    eventually("all lines out", || {
        std::fs::read_to_string(&log).is_ok_and(|text| text.ends_with(&last))
    })
    .await;
}

/// The model's load lines once its latest load ends with `last`.
async fn until_kept(s: &Setup, model: &str, last: &str) -> Value {
    for _ in 0..100 {
        let load = load_lines(&get(s, "/strata").await, model);
        if load["lines"].as_array().and_then(|l| l.last()) == Some(&json!(last)) {
            return load;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no load of {model} ending in {last} kept");
}

/// Starts `id`, lets it answer once its lines are out, and returns the
/// model's load lines.
async fn load(s: &Setup, setup: &SetupLeaves, id: &str, model: &str, lines: &[String]) -> Value {
    start(s, setup, id, lines, false).await;
    s.fake.strata_up.store(true, Ordering::SeqCst);
    until_kept(s, model, lines.last().unwrap()).await
}

fn lines(prefix: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("{prefix} {i}")).collect()
}

/// A model's load lines; before Strata's catalog is read the page knows
/// the model by its run config's lower-case name.
fn load_lines(view: &Value, id: &str) -> Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str().is_some_and(|m| m.eq_ignore_ascii_case(id)))
        .unwrap_or_else(|| panic!("{id} missing: {view}"))["load_lines"]
        .clone()
}

#[tokio::test]
async fn a_start_keeps_the_last_100_lines_of_the_models_load() {
    let s = setup("load-lines").await;
    let setup = installed(&s);
    assert_eq!(
        load_lines(&get(&s, "/strata").await, "IQ3_XXS"),
        Value::Null
    );
    let load = load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("loading", 120)).await;
    let kept = load["lines"].as_array().unwrap();
    assert_eq!(kept.len(), 100);
    assert_eq!(kept[0], "loading 21");
    assert_eq!(load["answered"], true);
    assert!(load["at"].as_u64().unwrap() > 0);
    let view = get(&s, "/strata").await;
    assert_eq!(load_lines(&view, "swift-IQ2_XS"), Value::Null);
}

#[tokio::test]
async fn a_later_load_replaces_only_its_models_lines() {
    let s = setup("load-lines-replace").await;
    let setup = installed(&s);
    load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("first", 2)).await;
    load(
        &s,
        &setup,
        "swift-iq2_xs",
        "swift-IQ2_XS",
        &lines("swift", 2),
    )
    .await;
    load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("second", 2)).await;
    let view = get(&s, "/strata").await;
    assert_eq!(
        load_lines(&view, "IQ3_XXS")["lines"],
        json!(["second 1", "second 2"])
    );
    assert_eq!(
        load_lines(&view, "swift-IQ2_XS")["lines"],
        json!(["swift 1", "swift 2"])
    );
}

#[tokio::test]
async fn a_strata_that_ends_before_answering_keeps_a_failed_load() {
    let s = setup("load-lines-crash").await;
    let setup = installed(&s);
    start(&s, &setup, "iq3_xxs", &lines("crash", 2), true).await;
    let load = until_kept(&s, "IQ3_XXS", "crash 2").await;
    assert_eq!(load["answered"], false);
}

#[tokio::test]
async fn a_load_the_service_stops_keeps_the_earlier_lines() {
    let s = setup("load-lines-stopped").await;
    let setup = installed(&s);
    load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("good", 2)).await;
    start(&s, &setup, "iq3_xxs", &lines("cut short", 2), false).await;
    post(&s, "/gaming", json!({"enabled": true})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let view = get(&s, "/strata").await;
    assert_eq!(
        load_lines(&view, "IQ3_XXS")["lines"],
        json!(["good 1", "good 2"])
    );
}

#[tokio::test]
async fn the_load_lines_survive_a_restart_of_the_service() {
    let s = setup("load-lines-restart").await;
    let setup = installed(&s);
    load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("loading", 2)).await;
    let again = Agent::new(s.agent.cfg.clone(), Box::new(FakePlatform(s.fake.clone())));
    let view = serde_json::to_value(strata::view(&again)).unwrap();
    assert_eq!(
        load_lines(&view, "IQ3_XXS")["lines"],
        json!(["loading 1", "loading 2"])
    );
}

#[tokio::test]
async fn a_deleted_model_takes_its_load_lines_along() {
    let s = setup("load-lines-delete").await;
    let setup = installed(&s);
    load(
        &s,
        &setup,
        "swift-iq2_xs",
        "swift-IQ2_XS",
        &lines("swift", 2),
    )
    .await;
    load(&s, &setup, "iq3_xxs", "IQ3_XXS", &lines("qwen", 2)).await;
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "delete", "model": "swift-IQ2_XS"}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    let kept = std::fs::read_to_string(s.dir.join("load-lines.json")).unwrap();
    assert!(!kept.contains("swift"), "{kept}");
    assert!(kept.contains("qwen 1"), "{kept}");
}
