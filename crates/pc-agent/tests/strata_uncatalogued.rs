//! An installed model that the catalog does not know, e.g. one Strata's own
//! app installed or one Strata dropped from its catalog: everything that
//! goes by its run config works, through the service's HTTP interface.

use crate::common;

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

/// A model id no catalog of the service knows; its download folder carries
/// the letter case setup gave it.
const ID: &str = "deepseek-v4.1-flash-IQ2_XXS";

fn config_json(gguf: &str) -> Value {
    json!({"exe": "component:strata-engine", "args": ["--native", gguf, "--max-context", "65536"],
        "port": 8080, "api_key": "key-0123456789abcdef", "reasoning_loop_recovery": "recover"})
}

/// The Terminal with IQ3_XXS (served) and [`ID`].
fn installed(s: &Setup) -> SetupLeaves {
    installed_serving(s, "iq3_xxs")
}

/// The Terminal with IQ3_XXS and [`ID`], `served` served.
fn installed_serving(s: &Setup, served: &str) -> SetupLeaves {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves
        .config("iq3_xxs", config_json("a.gguf"))
        .config(&ID.to_lowercase(), config_json("d.gguf"))
        .download("IQ3_XXS", "a.gguf", "0123456789")
        .download(ID, "d.gguf", "0123");
    let terminal = Terminal::of(cfg);
    let config = leaves.installation().config_path(served);
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&config, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
            ..Default::default()
        })
        .unwrap();
    leaves
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

async fn job(s: &Setup, body: Value) -> Value {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

#[tokio::test]
async fn the_page_shows_it_in_the_letter_case_of_its_download() {
    let s = setup("uncatalogued-view").await;
    installed(&s);
    let view = get(&s, "/strata").await;
    let m = model(&view, ID);
    assert_eq!(m["active"], false);
    assert_eq!(m["context"], 65536);
    assert_eq!(m["disk_bytes"], 4);
    assert_eq!(m["tunable"], true, "{m}");
    assert!(m.get("family").is_none_or(Value::is_null), "{m}");
}

#[tokio::test]
async fn it_can_be_activated_by_any_letter_case() {
    let s = setup("uncatalogued-activate").await;
    let leaves = installed(&s);
    let done = job(&s, json!({"kind": "activate", "model": ID.to_lowercase()})).await;
    assert_eq!(done["state"], "done", "{done}");
    let (_, args) = s.fake.started.lock().unwrap().clone().unwrap();
    let config = leaves.installation().config_path(ID);
    assert_eq!(args, Terminal::serve_args(&config, "0.0.0.0", "8080"));
    assert_eq!(model(&get(&s, "/strata").await, ID)["active"], true);
}

#[tokio::test]
async fn it_can_be_deleted() {
    let s = setup("uncatalogued-delete").await;
    let leaves = installed(&s);
    let done = job(&s, json!({"kind": "delete", "model": ID})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(leaves.installation().models(), ["iq3_xxs"]);
    assert_eq!(leaves.installation().disk_bytes(ID), 0);
}

#[tokio::test]
async fn its_context_changes_in_the_run_config() {
    let s = setup("uncatalogued-context").await;
    let leaves = installed(&s);
    let done = job(
        &s,
        json!({"kind": "set_context", "model": ID, "context": 32768}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let changed = leaves.installation().model(ID).unwrap();
    assert_eq!(changed.context(), Some(32768));
}

#[tokio::test]
async fn the_tune_runs_and_strata_s_refusal_is_the_job_s_error() {
    let s = setup("uncatalogued-tune").await;
    installed(&s);
    s.fake.command(
        "strata.cmd tune",
        FakeCommand {
            lines: vec![
                r#"{"ok": false, "error": {"code": "unsupported", "message": "no tuner for this model family"}}"#
                    .into(),
            ],
            exit: Some(6),
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "tune", "model": ID, "mode": "quick", "use": "chat"}),
    )
    .await;
    assert_eq!(done["state"], "failed", "{done}");
    let error = done["error"].as_str().unwrap_or_default();
    assert!(error.contains("no tuner for this model family"), "{done}");
}

#[tokio::test]
async fn the_speed_projection_is_refused_as_for_any_other_family() {
    let s = setup("uncatalogued-projection").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_speed_projection", "model": ID, "on": true}),
    )
    .await;
    assert_eq!(code, 400, "{answer}");
    assert!(answer.to_string().contains("only for"), "{answer}");
}

#[tokio::test]
async fn pictures_need_setup_and_so_the_catalog() {
    let s = setup("uncatalogued-pictures").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_pictures", "model": ID, "on": true}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
    assert!(answer.to_string().contains("catalog"), "{answer}");
}

#[tokio::test]
async fn a_model_that_is_not_installed_is_refused() {
    let s = setup("uncatalogued-missing").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "activate", "model": "nope"}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
    assert!(answer.to_string().contains("not installed"), "{answer}");
}

#[tokio::test]
async fn it_is_benchmarked_when_it_serves() {
    let s = setup("uncatalogued-bench").await;
    installed_serving(&s, &ID.to_lowercase());
    s.fake.command(
        "strata.cmd bench",
        FakeCommand {
            lines: vec![
                r#"{"schema_version": 1, "ok": true, "status": "complete", "summaries": [{"phase": "performance", "case_id": "short", "metric": "decode_tps", "median": 21.5}]}"#
                    .into(),
            ],
            ..FakeCommand::default()
        },
    );
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let result = &get(&s, "/strata").await["benchmark"];
    assert_eq!(result["model"], ID, "{result}");
    assert_eq!(result["result"], "21.5 Token/s");
}
