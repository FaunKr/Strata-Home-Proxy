//! Strata's catalog, read from the installed Strata, through the service's
//! HTTP interface: the page offers what it lists and jobs take its values.

use crate::common;

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};

fn config_json(gguf: &str) -> Value {
    json!({"exe": "component:strata-engine", "args": ["--native", gguf, "--max-context", "65536"],
        "port": 8080, "api_key": "key-0123456789abcdef", "reasoning_loop_recovery": "recover"})
}

/// The Terminal in build `version` with IQ3_XXS, served.
fn installed(s: &Setup, version: &str) {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, version);
    leaves
        .config("iq3_xxs", config_json("a.gguf"))
        .download("IQ3_XXS", "a.gguf", "0123456789");
    let terminal = Terminal::of(cfg);
    let config = leaves.installation().config_path("iq3_xxs");
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&config, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
        })
        .unwrap();
}

/// The catalog script answers with `lines` and `exit`.
fn catalog_answers(s: &Setup, lines: Vec<String>, exit: Option<i32>) {
    s.fake.command(
        "python catalog",
        FakeCommand {
            lines,
            exit,
            ..FakeCommand::default()
        },
    );
}

/// [`CATALOG`] with one more Qwen size, as a newer Strata may list it.
fn catalog_with_new_size() -> String {
    let mut catalog: Value = serde_json::from_str(CATALOG).unwrap();
    let qwen = catalog["catalog"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|f| f["family"] == "qwen")
        .unwrap();
    let mut size = qwen["sizes"][0].clone();
    size["model"] = json!("IQ4_NEW");
    size["id"] = json!("iq4_new");
    qwen["sizes"].as_array_mut().unwrap().push(size);
    catalog.to_string()
}

fn entry<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

fn family<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
}

fn add(size: &str, context: u32) -> Value {
    json!({"kind": "add_model", "family": "qwen", "size": size, "context": context})
}

#[tokio::test]
async fn the_page_offers_everything_strata_s_catalog_lists() {
    let s = setup("catalog-view").await;
    installed(&s, "0.2.2");
    let view = get(&s, "/strata").await;
    assert!(view.get("catalog_unreadable").is_none(), "{view}");
    let deepseek = entry(&view, "deepseek-v4.1-flash-UD-Q8_K_XL");
    assert_eq!(deepseek["download_gb"], 508.4);
    assert_eq!(deepseek["fits"], false, "Strata: does not fit");
    assert_eq!(entry(&view, "nvfp4-NVFP4")["fits"], true, "low-RAM mode");
    assert_eq!(entry(&view, "unsloth-Q8_0")["fits"], false, "slow here");
    let gyro = entry(&view, "gyro-Gyro-S");
    assert_eq!(gyro["ram_gb"], 28, "27.6 GB rounded up");
    assert_eq!(gyro["memory_with_vram"], true);
    assert_eq!(family(&view, "deepseek41")["title"], "DeepSeek-V4.1-Flash");
}

#[tokio::test]
async fn the_recommendation_and_contexts_are_strata_s() {
    let s = setup("catalog-recommend").await;
    installed(&s, "0.2.2");
    let view = get(&s, "/strata").await;
    assert_eq!(view["recommended"], "IQ3_S");
    assert_eq!(view["default_context"], 131072);
    assert_eq!(
        view["contexts"],
        json!([8192, 32768, 65536, 131072, 204800, 262144, 393216, 524288])
    );
}

#[tokio::test]
async fn the_speed_projection_stays_with_qwen_and_its_coder() {
    let s = setup("catalog-projection").await;
    installed(&s, "0.2.2");
    let view = get(&s, "/strata").await;
    let projection: Vec<_> = view["families"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["speed_projection"] == true)
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(projection, ["qwen", "coder"]);
}

#[tokio::test]
async fn the_catalog_is_read_once_for_each_strata() {
    let s = setup("catalog-once").await;
    installed(&s, "0.2.2");
    for _ in 0..3 {
        get(&s, "/strata").await;
    }
    assert_eq!(s.fake.runs_of("python catalog").len(), 1);
    let read = &s.fake.runs_of("python catalog")[0];
    let terminal = Terminal::of(&s.agent.cfg);
    assert_eq!(
        read.program,
        terminal.root().join("python").join("python.exe")
    );
}

#[tokio::test]
async fn an_unreadable_catalog_asks_for_a_strata_update_and_models_keep_working() {
    let s = setup("catalog-unreadable").await;
    installed(&s, "0.2.2");
    catalog_answers(
        &s,
        vec![
            "Traceback (most recent call last):".into(),
            "ModuleNotFoundError: No module named 'tools.strata_mcp'".into(),
        ],
        Some(1),
    );
    let view = get(&s, "/strata").await;
    assert_eq!(view["catalog"], json!([]), "{view}");
    assert!(view["catalog_unreadable"].is_string(), "{view}");
    assert_eq!(view["models"][0]["id"], "IQ3_XXS", "by its download");
    let (code, answer) = post(&s, "/strata/jobs", add("Q2_0", 65536)).await;
    assert_eq!(code, 409, "{answer}");
    assert!(
        answer
            .to_string()
            .contains("update Strata to choose a model"),
        "{answer}"
    );
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_context", "model": "iq3_xxs", "context": 32768}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
}

#[tokio::test]
async fn a_model_outside_strata_s_catalog_is_refused() {
    let s = setup("catalog-refuse").await;
    installed(&s, "0.2.2");
    for (body, why) in [
        (add("XXL", 65536), "not in Strata's catalog"),
        (add("Q2_0", 12345), "no context length 12345"),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body).await;
        assert_eq!(code, 400, "{answer}");
        assert!(answer.to_string().contains(why), "{answer}");
    }
    assert!(s.fake.runs_of("python").is_empty(), "no setup");
}

#[tokio::test]
async fn an_update_brings_the_new_strata_s_catalog() {
    let s = setup("catalog-update").await;
    installed(&s, "0.2.2");
    let view = get(&s, "/strata").await;
    assert!(!view["catalog"].to_string().contains("IQ4_NEW"), "{view}");
    catalog_answers(&s, vec![catalog_with_new_size()], None);
    let version = Terminal::of(&s.agent.cfg).root().join("VERSION");
    let control = r#"{"component": "control", "status": "updated", "changed": true, "installed_before": "0.2.2", "installed_after": "0.2.3"}"#;
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![format!(
                r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}}, "results": [{{"component": "engine", "status": "current", "changed": false}}, {control}], "exit_code": 0}}"#
            )],
            writes: vec![(version, "0.2.3\n".into())],
            ..FakeCommand::default()
        },
    );
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "update"})).await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    // The read after the job, without a page visit.
    eventually("the new Strata's catalog read", || {
        s.fake.runs_of("python catalog").len() == 2
    })
    .await;
    assert!(
        get(&s, "/strata").await["catalog"]
            .to_string()
            .contains("IQ4_NEW")
    );
    let (code, answer) = post(&s, "/strata/jobs", add("IQ4_NEW", 65536)).await;
    assert_eq!(code, 202, "{answer}");
}

#[tokio::test]
async fn a_failed_read_is_tried_again_after_the_next_job() {
    let s = setup("catalog-again").await;
    installed(&s, "0.2.2");
    catalog_answers(
        &s,
        vec!["Traceback (most recent call last):".into()],
        Some(1),
    );
    assert!(get(&s, "/strata").await["catalog_unreadable"].is_string());
    catalog_answers(&s, vec![CATALOG.trim_end().to_string()], None);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_context", "model": "IQ3_XXS", "context": 32768}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    eventually("the catalog read again", || {
        s.fake.runs_of("python catalog").len() == 2
    })
    .await;
    let view = get(&s, "/strata").await;
    assert!(view.get("catalog_unreadable").is_none(), "{view}");
    assert_eq!(view["recommended"], "IQ3_S");
}
