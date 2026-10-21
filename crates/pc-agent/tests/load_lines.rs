//! The load lines: each model's Strata output from a start by the service
//! until `/health` answers, kept on the PC and reported in `GET /strata`.

use crate::common;

use std::sync::atomic::Ordering;

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

/// Starts `id` through its start script, which prints `lines`, and lets it
/// answer `/health` only once all of them are out.
async fn load(s: &Setup, setup: &SetupLeaves, id: &str, lines: &[String]) {
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    s.fake.strata_up.store(false, Ordering::SeqCst);
    let script = setup.installation().run_script(id);
    s.fake.command(
        script.file_name().unwrap().to_str().unwrap(),
        FakeCommand {
            lines: lines.to_vec(),
            hang: true,
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
    s.fake.strata_up.store(true, Ordering::SeqCst);
    // `/status` asks `/health`, which ends the load.
    get(s, "/status").await;
}

fn lines(prefix: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("{prefix} {i}")).collect()
}

fn load_lines(view: &Value, id: &str) -> Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
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
    load(&s, &setup, "iq3_xxs", &lines("loading", 120)).await;
    let view = get(&s, "/strata").await;
    let load = load_lines(&view, "IQ3_XXS");
    let kept = load["lines"].as_array().unwrap();
    assert_eq!(kept.len(), 100);
    assert_eq!(kept[0], "loading 21");
    assert_eq!(kept[99], "loading 120");
    assert!(load["at"].as_u64().unwrap() > 0);
    assert_eq!(load_lines(&view, "swift-IQ2_XS"), Value::Null);
}

#[tokio::test]
async fn a_later_load_replaces_only_its_models_lines() {
    let s = setup("load-lines-replace").await;
    let setup = installed(&s);
    load(&s, &setup, "iq3_xxs", &lines("first", 2)).await;
    load(&s, &setup, "swift-iq2_xs", &lines("swift", 2)).await;
    load(&s, &setup, "iq3_xxs", &lines("second", 2)).await;
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
async fn the_load_lines_survive_a_restart_of_the_service() {
    let s = setup("load-lines-restart").await;
    let setup = installed(&s);
    load(&s, &setup, "iq3_xxs", &lines("loading", 2)).await;
    let again = Agent::new(s.agent.cfg.clone(), Box::new(FakePlatform(s.fake.clone())));
    let view = serde_json::to_value(strata::view(&again)).unwrap();
    assert_eq!(
        load_lines(&view, "IQ3_XXS")["lines"],
        json!(["loading 1", "loading 2"])
    );
}
