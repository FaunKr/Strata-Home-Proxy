//! Run configs that point into a Strata folder an update deleted (#398):
//! set up again by Strata's setup, their paths moved when setup cannot.

use crate::common;

use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{
    install::testing::SetupLeaves, managed::Managed, strata, terminal::Terminal,
};

/// The Control folder an earlier update made and a later one deleted.
const OLD_CONTROL: &str = "0.1.49.1";
const NEW_CONTROL: &str = "0.1.52";
const OLD_ENGINE: &str = "0.1.48-windows-x86_64-hip-rdna2";
const NEW_ENGINE: &str = "0.1.50-windows-x86_64-hip-rdna2";
const VECTOR: &str = "Qwen3.8-Flash-Next-experimental-speed-projection.gguf";

/// What `strata components engine --output json` prints (see the unit test
/// in `terminal.rs`), naming `root`.
fn engine_answer(root: &Path) -> String {
    json!({"schema_version": 1, "ok": true, "platform": "windows-x86_64", "backend": "hip",
        "backend_source": "requested", "installed": [], "problems": [],
        "engine": {"id": "strata-engine", "version": "0.1.50", "backend": "hip",
            "exe": root.join("strata.exe").display().to_string(),
            "root": root.display().to_string()},
        "usable": true, "notice": null})
    .to_string()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Pc {
    leaves: SetupLeaves,
    components: PathBuf,
}

impl Pc {
    fn control(&self, version: &str) -> PathBuf {
        self.components.join("control").join(version)
    }

    fn engine(&self, name: &str) -> PathBuf {
        self.components.join("engine").join(name)
    }

    fn config(&self, id: &str) -> Value {
        let model = self.leaves.installation().model(id).unwrap();
        Value::Object(model.fields().clone())
    }

    fn path(&self, id: &str) -> PathBuf {
        self.leaves.installation().config_path(id)
    }
}

/// The Strata Terminal after two Control updates: Control `0.1.52` runs
/// (`active.json`), `0.1.49.1` is gone; engine `0.1.50` is there, `0.1.48`
/// gone. The run configs of `ids`, served the first, were written by setup
/// under Control `0.1.49.1`, as seen on a PC.
fn updated_twice(s: &Setup, ids: &[&str]) -> Pc {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    let components = cfg.strata_components.clone();
    let control = components.join("control");
    write(
        &control.join("active.json"),
        &json!({"folder": NEW_CONTROL, "version": "0.2.6"}).to_string(),
    );
    let new = control.join(NEW_CONTROL);
    for file in [
        "bin/strata.cmd",
        "python/python.exe",
        "strata.py",
        "strata_control/__init__.py",
        "data/expert-profile.bin",
    ] {
        write(&new.join(file), "");
    }
    write(
        &new.join("data")
            .join("experimental-speed-projection")
            .join(VECTOR),
        "",
    );
    write(&new.join("VERSION"), "0.2.6\n");
    write(
        &components
            .join("engine")
            .join(NEW_ENGINE)
            .join("strata-vision.exe"),
        "",
    );
    let pc = Pc { leaves, components };
    for id in ids {
        pc.leaves
            .config(id, stale_config(&pc, cfg.strata_data_dir.as_path()));
    }
    let terminal = Terminal::of(cfg);
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&pc.path(ids[0]), "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
            ..Default::default()
        })
        .unwrap();
    // Strata's Python has jsonschema: no pip among setup's runs.
    s.fake.command("python jsonschema", FakeCommand::default());
    pc
}

/// A run config as setup wrote it under Control `0.1.49.1` (the PC's, with
/// the user's folder replaced).
fn stale_config(pc: &Pc, data: &Path) -> Value {
    let old = pc.control(OLD_CONTROL);
    let profile = old
        .join("data")
        .join("expert-profile.bin")
        .display()
        .to_string();
    let vector = format!(
        "{}:1.0",
        old.join("data")
            .join("experimental-speed-projection")
            .join(VECTOR)
            .display()
    );
    let args = json!([
        "--pack",
        data.join("packs").join("iq3_xxs"),
        "--native",
        data.join("models").join("IQ3_XXS").join("a.gguf"),
        "--expert-profile",
        profile,
        "--max-context",
        "65536",
        "--kv",
        "int8",
        "--vision",
        "--control-vector-scaled",
        vector,
        "--control-vector-layer-range",
        "4",
        "44",
        "--cvec-mode",
        "project",
        "--cvec-dir",
        "per-layer"
    ]);
    json!({
        "exe": "component:strata-engine",
        "args": args,
        "args_written": args,
        "cwd": old.display().to_string(),
        "port": 8080,
        "backend": "hip",
        "api_key": "key-0123456789abcdef",
        "vision": {"exe": pc.engine(OLD_ENGINE).join("strata-vision.exe").display().to_string(),
            "mmproj": data.join("models").join("mmproj.gguf"), "gpu": false, "max_tokens": 300, "threads": 8},
        "user_args": ["--kv", "k8v4", format!("--expert-profile={profile}")],
    })
}

/// Setup rewrites the run config of `id` with Control `0.1.52`'s folder.
fn setup_writes(s: &Setup, pc: &Pc, id: &str, exit: Option<i32>) {
    let mut fresh = stale_config(pc, &s.agent.cfg.strata_data_dir);
    fresh["cwd"] = json!(pc.control(NEW_CONTROL).display().to_string());
    fresh["vision"]["exe"] = json!(
        pc.engine(NEW_ENGINE)
            .join("strata-vision.exe")
            .display()
            .to_string()
    );
    let profile = pc
        .control(NEW_CONTROL)
        .join("data")
        .join("expert-profile.bin");
    fresh["args"][5] = json!(profile.display().to_string());
    fresh["args"][12] = json!(format!(
        "{}:1.0",
        pc.control(NEW_CONTROL)
            .join("data")
            .join("experimental-speed-projection")
            .join(VECTOR)
            .display()
    ));
    fresh["args_written"] = fresh["args"].clone();
    fresh["user_args"][2] = json!(format!("--expert-profile={}", profile.display()));
    s.fake.command(
        "python",
        FakeCommand {
            lines: lines(&["setting up ..."]),
            pause: Duration::from_millis(100),
            exit,
            writes: vec![(pc.path(id), serde_json::to_string_pretty(&fresh).unwrap())],
            ..FakeCommand::default()
        },
    );
}

fn engine_says(s: &Setup, line: &str, exit: Option<i32>) {
    s.fake.command(
        "strata.cmd components",
        FakeCommand {
            lines: vec![line.to_string()],
            exit,
            ..FakeCommand::default()
        },
    );
}

/// Setup's runs, not the catalog's or jsonschema's.
fn setups(s: &Setup) -> Vec<Vec<String>> {
    s.fake
        .runs_of("python")
        .into_iter()
        .filter(|c| c.args.first().is_some_and(|a| a.ends_with("setup.py")))
        .map(|c| c.args)
        .collect()
}

/// Every text of the run config `id` that names a Control or engine folder.
fn folder_paths(pc: &Pc, id: &str) -> Vec<String> {
    let config = pc.config(id);
    let mut texts = vec![config["cwd"].clone(), config["vision"]["exe"].clone()];
    for key in ["args", "args_written", "user_args"] {
        texts.extend(config[key].as_array().unwrap().iter().cloned());
    }
    let components = pc.components.display().to_string();
    texts
        .iter()
        .filter_map(|t| t.as_str())
        .filter(|t| t.contains(&components))
        .map(String::from)
        .collect()
}

fn assert_moved(pc: &Pc, id: &str, engine_too: bool) {
    let paths = folder_paths(pc, id);
    assert_eq!(paths.len(), 7, "{paths:?}");
    let new_control = pc.control(NEW_CONTROL).display().to_string();
    let new_engine = pc.engine(NEW_ENGINE).display().to_string();
    for path in &paths {
        let engine = path.contains(&pc.components.join("engine").display().to_string());
        let want = if engine && !engine_too {
            pc.engine(OLD_ENGINE).display().to_string()
        } else if engine {
            new_engine.clone()
        } else {
            new_control.clone()
        };
        assert!(path.contains(&want), "{path} not in {want}");
    }
    let config = pc.config(id);
    assert_eq!(
        config["args"], config["args_written"],
        "a pair that was equal stays equal"
    );
    assert_eq!(config["args"][7], "65536", "the rest stays");
}

#[tokio::test]
async fn at_the_service_start_setup_sets_a_stale_model_up_again_before_strata_starts() {
    let s = setup("repair-at-start").await;
    let pc = updated_twice(&s, &["iq3_xxs"]);
    setup_writes(&s, &pc, "iq3_xxs", None);
    // The program before stopped Strata for its own update.
    std::fs::write(s.agent.cfg.state_dir().join("strata-after-update"), "").unwrap();
    s.fake.strata_up.store(false, Ordering::SeqCst);
    let rest = strata::manage_configs_at_start(&s.agent).await;
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        0,
        "not before the repair"
    );
    rest.await;
    let job = get(&s, "/strata/job").await;
    assert_eq!(job["kind"], "repair_run_configs", "{job}");
    assert_eq!(job["state"], "done", "{job}");
    let runs = setups(&s);
    assert_eq!(runs.len(), 1, "{runs:?}");
    let args = runs[0].join(" ");
    for wanted in [
        "--family qwen --model IQ3_XXS",
        "--context 65536",
        "--vision yes",
        "--experimental-speed-projection on",
    ] {
        assert!(args.contains(wanted), "{wanted} missing: {args}");
    }
    assert_moved(&pc, "iq3_xxs", true);
    assert_eq!(
        pc.config("iq3_xxs")["reasoning_loop_recovery"],
        "recover",
        "managed after setup"
    );
    assert_eq!(
        s.fake.starts.load(Ordering::SeqCst),
        1,
        "Strata starts after the repair"
    );
}

#[tokio::test]
async fn when_setup_fails_the_paths_move_to_the_folders_strata_uses_now() {
    let s = setup("repair-setup-fails").await;
    let pc = updated_twice(&s, &["iq3_xxs"]);
    setup_writes(&s, &pc, "iq3_xxs", Some(1));
    engine_says(&s, &engine_answer(&pc.engine(NEW_ENGINE)), None);
    strata::manage_configs_at_start(&s.agent).await.await;
    let job = get(&s, "/strata/job").await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["result"], Value::Null, "nothing left: {job}");
    assert_eq!(setups(&s).len(), 1);
    assert_moved(&pc, "iq3_xxs", true);
    let asked = &s.fake.runs_of("strata.cmd")[0].args;
    assert_eq!(
        asked,
        &[
            "components",
            "engine",
            "--output",
            "json",
            "--backend",
            "hip"
        ]
    );
}

#[tokio::test]
async fn a_model_not_in_strata_s_catalog_gets_its_paths_moved_without_setup() {
    let s = setup("repair-uncatalogued").await;
    let pc = updated_twice(&s, &["mine-q4"]);
    engine_says(&s, &engine_answer(&pc.engine(NEW_ENGINE)), None);
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(get(&s, "/strata/job").await["state"], "done");
    assert!(setups(&s).is_empty());
    assert_moved(&pc, "mine-q4", true);
}

#[tokio::test]
async fn without_strata_s_engine_folder_the_control_paths_still_move() {
    let s = setup("repair-no-engine").await;
    let pc = updated_twice(&s, &["mine-q4"]);
    engine_says(
        &s,
        r#"{"schema_version": 1, "ok": false, "engine": null, "usable": false, "notice": "no engine for this PC"}"#,
        Some(5),
    );
    strata::manage_configs_at_start(&s.agent).await.await;
    let job = get(&s, "/strata/job").await;
    assert_eq!(job["state"], "done", "{job}");
    assert!(job["result"].as_str().unwrap().contains("mine-q4"), "{job}");
    assert_moved(&pc, "mine-q4", false);
}

#[tokio::test]
async fn run_configs_without_stale_paths_get_no_job_and_no_write() {
    let s = setup("repair-nothing").await;
    let pc = updated_twice(&s, &["iq3_xxs"]);
    let mut fine = pc.config("iq3_xxs");
    fine["cwd"] = json!(pc.control(NEW_CONTROL).display().to_string());
    fine["args"] = json!(["--native", "a.gguf"]);
    fine["args_written"] = fine["args"].clone();
    fine["user_args"] = json!([]);
    fine["vision"]["exe"] = json!(
        pc.engine(NEW_ENGINE)
            .join("strata-vision.exe")
            .display()
            .to_string()
    );
    fine["reasoning_loop_recovery"] = json!("recover");
    pc.leaves.config("iq3_xxs", fine.clone());
    let modified = std::fs::metadata(pc.path("iq3_xxs"))
        .unwrap()
        .modified()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(get(&s, "/strata/job").await, Value::Null);
    assert!(setups(&s).is_empty());
    assert_eq!(
        std::fs::metadata(pc.path("iq3_xxs"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
}

#[tokio::test]
async fn in_gaming_mode_the_paths_move_at_the_start_without_a_job() {
    let s = setup("repair-gaming").await;
    let pc = updated_twice(&s, &["iq3_xxs"]);
    setup_writes(&s, &pc, "iq3_xxs", None);
    engine_says(&s, &engine_answer(&pc.engine(NEW_ENGINE)), None);
    s.agent.set_gaming(true);
    strata::manage_configs_at_start(&s.agent).await.await;
    assert_eq!(get(&s, "/strata/job").await, Value::Null);
    assert!(setups(&s).is_empty());
    assert_moved(&pc, "iq3_xxs", true);
}

#[tokio::test]
async fn a_strata_update_sets_stale_models_up_again_before_strata_starts_again() {
    let s = setup("repair-update").await;
    let pc = updated_twice(&s, &["iq3_xxs", "coder-iq1_m"]);
    setup_writes(&s, &pc, "iq3_xxs", None);
    engine_says(&s, &engine_answer(&pc.engine(NEW_ENGINE)), None);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: lines(&[r#"{"schema_version": 1, "ok": true, "feed": {"source": "f", "sequence": 42, "channel": "stable", "key_id": "k"}, "results": [{"component": "engine", "status": "current", "changed": false}, {"component": "control", "status": "updated", "changed": true}], "exit_code": 0}"#]),
            ..FakeCommand::default()
        },
    );
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "update"})).await;
    assert_eq!(code, 202, "{answer}");
    let done = finished(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(setups(&s).len(), 2, "one setup per stale model");
    // coder-IQ1_M's setup wrote IQ3_XXS's config: it still had stale paths
    // and they moved.
    assert_moved(&pc, "iq3_xxs", true);
    assert_moved(&pc, "coder-iq1_m", true);
}

#[tokio::test]
async fn before_a_start_the_served_run_config_gets_its_paths_moved_without_setup() {
    let s = setup("repair-before-start").await;
    let pc = updated_twice(&s, &["iq3_xxs"]);
    s.agent.strata().start();
    assert!(setups(&s).is_empty());
    assert!(s.fake.runs_of("strata.cmd").is_empty(), "no program asked");
    // No engine folder named yet: engine paths stay until a repair asks.
    assert_moved(&pc, "iq3_xxs", false);
}
