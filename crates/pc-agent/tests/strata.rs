//! Strata management through the service's HTTP interface, with a fake
//! install script, a fake Strata Terminal and a fake setup.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

/// What `strata update --output json` prints for the engine's `status`
/// (`strata_cli/update_cmd.py`, `run`, Strata 011c9e6).
fn engine_answer(status: &str) -> String {
    format!(
        r#"{{"schema_version": 1, "ok": true, "feed": {{"source": "f", "sequence": 41, "channel": "stable", "key_id": "k"}}, "results": [{{"component": "engine", "status": "{status}", "installed_before": null, "changed": true, "note": "The feed has no engine package for this PC.", "reasons": ["hip gfx1030: no package"]}}], "exit_code": 0}}"#
    )
}

/// The install script puts the Strata Terminal in place, its update brings
/// the engine and setup in its install mode sets up IQ3_XXS.
fn installable(s: &Setup) {
    let terminal = s.agent.cfg.strata_home.join("terminal").join("current");
    let install = s.agent.cfg.strata_home.join("install");
    let data = s.agent.cfg.strata_data_dir.clone();
    s.fake.command(
        "powershell",
        FakeCommand {
            lines: lines(&["Strata Terminal installer", "Installed. Run: strata launch"]),
            creates: vec![
                (terminal.join("bin").join("strata.cmd"), "@echo off".into()),
                (terminal.join("strata.py"), String::new()),
                (terminal.join("setup.py"), String::new()),
                (terminal.join("python").join("python.exe"), String::new()),
                (terminal.join("VERSION"), "0.2.2\n".into()),
            ],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![engine_answer("updated")],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "python",
        FakeCommand {
            lines: lines(&[
                "=== Step 1: Checking this PC ===",
                "=== Step 4: Downloading the model ===",
                "  Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf: 37.9 / 75.8 GB (50%)",
            ]),
            creates: vec![
                (
                    install.join("strata-iq3_xxs.json"),
                    json!({"exe": "component:strata-engine", "args": ["--native"]}).to_string(),
                ),
                (
                    data.join("models").join("IQ3_XXS").join("a.gguf"),
                    "0123456789".into(),
                ),
            ],
            ..FakeCommand::default()
        },
    );
}

fn install() -> Value {
    json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 65536, "api_key": KEY})
}

fn env<'a>(spec: &'a CommandSpec, name: &str) -> Option<&'a str> {
    spec.env
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn values_outside_the_catalog_are_refused() {
    let s = setup("strata-catalog").await;
    for body in [
        json!({"kind": "install", "family": "qwen", "size": "XXL", "context": 65536, "api_key": KEY}),
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 12345, "api_key": KEY}),
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 65536, "api_key": KEY, "args": "--evil"}),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
        assert!(code == 400 || code == 422, "{body}: {code} {answer}");
    }
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn install_sets_up_the_strata_terminal_and_serves_the_new_model() {
    let s = setup("strata-install").await;
    installable(&s);
    let (code, job) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202, "{job}");
    assert!(!job.to_string().contains(KEY), "{job}");
    let job = finished(&s).await;
    assert_eq!(job["state"], "done", "{job}");
    let cfg = &s.agent.cfg;
    let home = cfg.strata_home.display().to_string();
    let install = cfg.strata_home.join("install");

    // Strata's own install script, into the service's folder; no Git.
    let script = &s.fake.runs_of("powershell")[0];
    assert_eq!(
        script.args.last().unwrap(),
        "irm 'https://example.org/install.ps1' | iex"
    );
    assert_eq!(env(script, "STRATA_DATA_DIR"), Some(home.as_str()));
    assert!(s.fake.runs_of("git").is_empty());
    assert!(s.fake.runs_of("winget").is_empty());

    // The engine from the Terminal's update, for the service's run configs.
    let terminal = cfg.strata_home.join("terminal").join("current");
    let update = &s.fake.runs_of("strata.cmd")[0];
    assert_eq!(update.program, terminal.join("bin").join("strata.cmd"));
    assert_eq!(update.args, ["update", "--no-control", "--output", "json"]);
    assert_eq!(
        env(update, "STRATA_INSTALL_DIR"),
        Some(install.display().to_string().as_str())
    );

    // Setup in its install mode, with the fixed values.
    let setup = &s.fake.runs_of("python")[0];
    assert_eq!(setup.program, terminal.join("python").join("python.exe"));
    assert_eq!(env(setup, "STRATA_CONTROL_INSTALL"), Some("1"));
    assert_eq!(
        env(setup, "STRATA_INSTALL_DIR"),
        Some(install.display().to_string().as_str())
    );
    let data = cfg.strata_data_dir.display().to_string();
    assert_eq!(
        setup.args,
        [
            terminal.join("setup.py").display().to_string().as_str(),
            "--yes",
            "--no-start",
            "--no-browser",
            "--family",
            "qwen",
            "--model",
            "IQ3_XXS",
            "--context",
            "65536",
            "--vision",
            "no",
            "--host",
            "0.0.0.0",
            "--port",
            "8080",
            "--data-dir",
            &data,
            "--experimental-speed-projection",
            "off",
        ]
    );

    // From now on the service serves the new model's run config, with a
    // key of its own and not the proxy's.
    let config = install.join("strata-iq3_xxs.json");
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, terminal.join("bin").join("strata.cmd"));
    assert_eq!(
        args,
        [
            "serve",
            "--profile",
            &config.display().to_string(),
            "--host",
            "0.0.0.0",
            "--port",
            "8080"
        ]
    );
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped, ["llama-server.exe"], "the old Strata ends first");
    let key = terminal_folder(&s)
        .installation()
        .model("iq3_xxs")
        .unwrap()
        .fields()["api_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(key, KEY);
    get(&s, "/status").await;
    assert_eq!(
        s.fake.health_auth.lock().unwrap().as_deref(),
        Some(format!("Bearer {key}").as_str())
    );
    // Stopping ends every program of the Terminal's folder.
    s.agent.set_gaming(true);
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped.len(), 1);
    assert!(stopped[0].starts_with(&home), "{stopped:?}");
    assert!(stopped[0].ends_with(['/', '\\']), "{stopped:?}");

    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], true);
    assert_eq!(view["install"], "terminal");
    assert_eq!(view["version"], "build 0.2.2", "before the first check");
    assert_eq!(view["models"][0]["id"], "IQ3_XXS");
    assert_eq!(view["models"][0]["active"], true);
    assert_eq!(view["models"][0]["disk_bytes"], 10);
    assert!(!view.to_string().contains(KEY));
}

#[tokio::test]
async fn without_an_engine_for_this_pc_nothing_is_downloaded() {
    let s = setup("strata-no-engine").await;
    installable(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: vec![engine_answer("none-for-this-pc")],
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(
        job["error"],
        "The feed has no engine package for this PC. (hip gfx1030: no package)"
    );
    assert!(s.fake.runs_of("python").is_empty(), "no model download");
    assert!(s.fake.started.lock().unwrap().is_none(), "no start");
}

#[tokio::test]
async fn a_failed_install_script_fails_the_job_with_its_lines() {
    let s = setup("strata-script-fails").await;
    installable(&s);
    s.fake.command(
        "powershell",
        FakeCommand {
            lines: lines(&[
                "Strata Terminal installer",
                "strata installer: the download failed (timeout).",
            ]),
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(
        job["error"]
            .as_str()
            .unwrap()
            .contains("powershell ended with code 1"),
        "{job}"
    );
    assert!(job.to_string().contains("the download failed"), "{job}");
    assert!(s.fake.runs_of("strata.cmd").is_empty());
}

#[tokio::test]
async fn an_installed_terminal_is_not_installed_again() {
    let s = setup("strata-terminal-there").await;
    installable(&s);
    SetupLeaves::terminal(
        &s.agent.cfg.strata_home,
        &s.agent.cfg.strata_data_dir,
        "0.2.2",
    );
    post(&s, "/strata/jobs", install()).await;
    assert_eq!(finished(&s).await["state"], "done");
    assert!(s.fake.runs_of("powershell").is_empty());
    assert_eq!(s.fake.runs_of("strata.cmd").len(), 1, "the engine still");
}

#[tokio::test]
async fn a_strata_folder_from_source_keeps_its_own_setup() {
    let s = setup("strata-source-install").await;
    let cfg = &s.agent.cfg;
    SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    post(&s, "/strata/jobs", install()).await;
    finished(&s).await;
    assert_eq!(s.fake.runs_of("START-HERE.bat").len(), 1);
    assert!(s.fake.runs_of("powershell").is_empty());
    assert!(s.fake.runs_of("python").is_empty());
    assert_eq!(get(&s, "/strata").await["install"], "source");
}

#[tokio::test]
async fn install_sets_loop_recovery_in_the_new_config() {
    let s = setup("strata-install-loop").await;
    installable(&s);
    let (code, job) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202, "{job}");
    assert_eq!(finished(&s).await["state"], "done");
    let model = terminal_folder(&s).installation().model("iq3_xxs").unwrap();
    let cfg = Value::Object(model.fields().clone());
    assert_eq!(cfg["reasoning_loop_recovery"], "recover", "{cfg}");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn install_turns_the_experimental_speed_projection_on_or_off() {
    for (wanted, value) in [(true, "on"), (false, "off")] {
        let s = setup(&format!("strata-esp-{value}")).await;
        installable(&s);
        let mut body = install();
        body["experimental_speed_projection"] = json!(wanted);
        let (code, job) = post(&s, "/strata/jobs", body).await;
        assert_eq!(code, 202, "{job}");
        assert_eq!(finished(&s).await["state"], "done");
        let args = &s.fake.runs_of("python")[0].args;
        assert!(
            args.ends_with(&["--experimental-speed-projection".into(), value.into()]),
            "{args:?}"
        );
    }
}

#[tokio::test]
async fn the_speed_projection_is_refused_for_other_families() {
    let s = setup("strata-esp-swift").await;
    let body = json!({"kind": "install", "family": "swift", "size": "IQ2_XS", "context": 65536,
        "api_key": KEY, "experimental_speed_projection": true});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 400, "{answer}");
    assert!(
        answer.to_string().contains("not for swift-IQ2_XS"),
        "{answer}"
    );
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_second_job_is_refused_while_one_runs() {
    let s = setup("strata-second").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    assert_eq!(post(&s, "/strata/jobs", install()).await.0, 202);
    job_until(&s, "setup running", |_| {
        !s.fake.runs_of("python").is_empty()
    })
    .await;
    let (code, answer) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 409);
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("running already"),
        "{answer}"
    );
}

#[tokio::test]
async fn a_running_job_keeps_the_pc_awake_and_the_watchdog_away() {
    let s = setup("strata-awake").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "setup running", |_| {
        !s.fake.runs_of("python").is_empty()
    })
    .await;

    let (code, answer) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert_eq!(answer["reason"], "A Strata job is running");
    assert_eq!(get(&s, "/status").await["strata_job"], true);

    s.fake.strata_up.store(false, Ordering::SeqCst);
    s.agent.watchdog_tick().await;
    s.agent.watchdog_tick().await;
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn gaming_mode_refuses_jobs() {
    let s = setup("strata-gaming").await;
    installable(&s);
    s.agent.set_gaming(true);
    let (code, answer) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 409);
    assert!(
        answer["reason"].as_str().unwrap().contains("Gaming"),
        "{answer}"
    );
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_download_that_does_not_fit_is_refused() {
    let s = setup("strata-space").await;
    installable(&s);
    *s.fake.disk_free.lock().unwrap() = Some(50_000_000_000);
    let (code, answer) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 409);
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("Not enough space"),
        "{answer}"
    );
}

#[tokio::test]
async fn a_failed_setup_shows_its_error_and_last_lines() {
    let s = setup("strata-fail").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            lines: lines(&[
                "=== Step 2: Python packages ===",
                "  [X] pip failed",
                "       no network",
            ]),
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed");
    assert!(
        job["error"].as_str().unwrap().contains("pip failed"),
        "{job}"
    );
    // Setup's lines come last, after the install script's and the engine's.
    let shown = job["lines"].as_array().unwrap();
    assert_eq!(shown[shown.len() - 3], "=== Step 2: Python packages ===");
    assert!(
        s.fake.started.lock().unwrap().is_none(),
        "no start after a failure"
    );
}

#[tokio::test]
async fn the_catalog_marks_what_fits_and_recommends_setups_pick() {
    let s = setup("strata-view").await;
    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], false);
    assert_eq!(view["recommended"], "IQ3_S", "64 GB of RAM, no card");
    assert_eq!(view["default_context"], 65536);
    let entry = |id: &str| {
        view["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(entry("IQ3_S")["fits"], true);
    assert_eq!(entry("unsloth-Q8_0")["fits"], false);
    assert_eq!(entry("unsloth-Q8_0")["ram_gb"], 136);
    let projection: Vec<_> = view["families"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["speed_projection"] == true)
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(projection, ["qwen", "coder"]);
}

/// A PC with this much RAM and a card with this much VRAM, in GiB as setup
/// counts them; no card for 0.
fn pc_with(s: &Setup, ram_gib: f64, vram_gib: f64) {
    let gib = |v: f64| (v * (1u64 << 30) as f64) as u64;
    s.fake.ram.store(gib(ram_gib), Ordering::SeqCst);
    s.fake.hardware.lock().unwrap().gpu = (vram_gib > 0.0).then(|| Gpu {
        vram_total_bytes: Some(gib(vram_gib)),
        ..Gpu::default()
    });
}

fn catalog_entry(view: &Value, id: &str) -> Value {
    view["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing: {view}"))
        .clone()
}

/// Setup's `MEMORY_TIERS` (Strata aa82a886) by RAM + VRAM, below them the
/// older rule by RAM alone.
#[tokio::test]
async fn the_recommendation_follows_setups_memory_tiers() {
    let s = setup("strata-tiers").await;
    for (ram, vram, expected) in [
        (64.0, 0.0, json!("IQ3_S")),
        (32.0, 30.0, json!("IQ3_S")),
        (61.9, 0.0, json!("IQ3_XXS")),
        (32.0, 14.0, json!("IQ3_XXS")),
        (45.9, 0.0, json!("swift-IQ2_XS")),
        (24.0, 14.0, json!("swift-IQ2_XS")),
        (37.9, 0.0, json!("coder-IQ1_M")),
        (28.0, 8.0, json!("coder-IQ1_M")),
        (16.0, 16.0, Value::Null),
    ] {
        pc_with(&s, ram, vram);
        let view = get(&s, "/strata").await;
        assert_eq!(
            view["recommended"], expected,
            "{ram} GB RAM + {vram} GB VRAM"
        );
        if let Some(id) = expected.as_str() {
            // Setup's low-RAM mode starts it where RAM alone is short.
            assert_eq!(
                catalog_entry(&view, id)["fits"],
                true,
                "{ram} + {vram}: {id}"
            );
        }
    }
}

#[tokio::test]
async fn the_page_says_the_vram_it_counts() {
    let s = setup("strata-vram").await;
    pc_with(&s, 32.0, 16.0);
    assert_eq!(get(&s, "/strata").await["vram_gb"], 16.0);
    pc_with(&s, 32.0, 0.0);
    assert_eq!(get(&s, "/strata").await["vram_gb"], Value::Null);
}

#[tokio::test]
async fn gyro_fits_from_setups_own_figure() {
    let s = setup("strata-gyro-edge").await;
    pc_with(&s, 16.0, 11.5);
    let view = get(&s, "/strata").await;
    assert_eq!(
        catalog_entry(&view, "gyro-Gyro-S")["fits"],
        false,
        "27.5 GB"
    );
    pc_with(&s, 16.0, 11.7);
    let view = get(&s, "/strata").await;
    assert_eq!(catalog_entry(&view, "gyro-Gyro-S")["fits"], true, "27.7 GB");
}

#[tokio::test]
async fn gyro_fits_by_ram_and_vram_and_is_never_recommended() {
    let s = setup("strata-gyro-view").await;
    pc_with(&s, 16.0, 16.0);
    let view = get(&s, "/strata").await;
    let gyro_s = catalog_entry(&view, "gyro-Gyro-S");
    assert_eq!(gyro_s["family"], "gyro");
    assert_eq!(gyro_s["download_gb"], 58.5);
    assert_eq!(gyro_s["ram_gb"], 28);
    assert_eq!(gyro_s["memory_with_vram"], true);
    assert_eq!(gyro_s["fits"], true, "16 + 16 GB: {gyro_s}");
    let gyro_m = catalog_entry(&view, "gyro-Gyro-M");
    assert_eq!(gyro_m["download_gb"], 92.0);
    assert_eq!(gyro_m["ram_gb"], 35);
    assert_eq!(gyro_m["fits"], false, "{gyro_m}");
    assert_eq!(
        catalog_entry(&view, "coder-IQ1_M")["memory_with_vram"],
        false
    );
    assert_eq!(
        catalog_entry(&view, "coder-IQ1_M")["fits"],
        false,
        "RAM alone"
    );
    assert_eq!(view["recommended"], Value::Null, "{view}");
    let gyro = view["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "gyro")
        .unwrap();
    assert_eq!(gyro["title"], "Qwen3.8-Flash-Next (Gyro)");
    assert_eq!(gyro["speed_projection"], false);
}

#[tokio::test]
async fn gyro_installs_through_setup_like_any_family() {
    let s = setup("strata-gyro-install").await;
    installable(&s);
    let body = json!({"kind": "install", "family": "gyro", "size": "Gyro-S", "context": 65536,
        "api_key": KEY});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(&s).await;
    let args = &s.fake.runs_of("python")[0].args;
    assert!(
        args.windows(4)
            .any(|w| w == ["--family", "gyro", "--model", "Gyro-S"]),
        "{args:?}"
    );
    assert!(
        !args.contains(&"--experimental-speed-projection".to_string()),
        "{args:?}"
    );
    let body = json!({"kind": "install", "family": "gyro", "size": "Gyro-M", "context": 65536,
        "api_key": KEY, "experimental_speed_projection": true});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 400);
}

// --- Progress, cancel, resume (#30) ---

#[tokio::test]
async fn progress_comes_from_setups_output() {
    let s = setup("strata-progress").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            lines: lines(&[
                "=== Step 4: Downloading the model ===",
                "  model.gguf: 10.0 / 80.0 GB (12%)",
            ]),
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = job_until(&s, "download progress", |j| j["percent"] == 12).await;
    assert_eq!(job["step"], "4/7: Downloading the model");
    assert_eq!(job["done_bytes"], 10_000_000_000u64);
    assert_eq!(job["total_bytes"], 80_000_000_000u64);
}

#[tokio::test]
async fn cancel_ends_the_running_command() {
    let s = setup("strata-cancel").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "setup running", |_| {
        !s.fake.runs_of("python").is_empty()
    })
    .await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    assert_eq!(finished(&s).await["state"], "cancelled");
    assert_eq!(s.fake.cancels.load(Ordering::SeqCst), 1);
    // Nothing runs any more.
    assert_eq!(post(&s, "/strata/job/cancel", json!({})).await.0, 409);
}

#[tokio::test]
async fn a_job_running_at_a_restart_is_interrupted() {
    let s = setup("strata-restart").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "setup running", |_| {
        !s.fake.runs_of("python").is_empty()
    })
    .await;
    let again = s.restarted();
    let job = again.status().await;
    assert!(!job.strata_job);
    let addr = serve(router(again)).await;
    let job: Value = client()
        .get(format!("http://{addr}/strata/job"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(job["state"], "interrupted");
    assert_eq!(job["model"], "IQ3_XXS");
}

#[tokio::test]
async fn gaming_on_cancels_a_running_job() {
    let s = setup("strata-gaming-cancel").await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "setup running", |_| {
        !s.fake.runs_of("python").is_empty()
    })
    .await;
    s.agent.set_gaming(true);
    assert_eq!(finished(&s).await["state"], "cancelled");
    // A new start with the same values continues the download.
    s.agent.set_gaming(false);
    installable(&s);
    let (code, _) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202);
    assert_eq!(finished(&s).await["state"], "done");
}

// --- Forced sleep and forced Strata restart cancel a running job ---

/// An install whose setup runs until it is cancelled.
async fn hanging_install(s: &Setup) {
    installable(s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(s, "/strata/jobs", install()).await;
    job_until(s, "setup running", |_| !s.fake.runs_of("python").is_empty()).await;
}

#[tokio::test]
async fn forced_sleep_cancels_a_running_job_and_its_download_continues_later() {
    let s = setup("strata-force-sleep").await;
    hanging_install(&s).await;
    let (code, body) = post(&s, "/sleep/force", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["cancelled_job"], "install");
    assert_eq!(finished(&s).await["state"], "cancelled");
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
    // A new start with the same values continues the download.
    installable(&s);
    let (code, _) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202);
    assert_eq!(finished(&s).await["state"], "done");
}

#[tokio::test]
async fn forced_restart_kills_strata_once_the_cancelled_job_has_ended() {
    let s = setup("strata-force-restart").await;
    hanging_install(&s).await;
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, json!({"cancelled_job": "install", "started": true}));
    // The answer comes after the job's end.
    assert_eq!(get(&s, "/strata/job").await["state"], "cancelled");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

/// An install whose setup runs on even when cancelled, like a stuck process.
async fn stuck_install(s: &Setup) {
    installable(s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ignores_cancel: true,
            ..FakeCommand::default()
        },
    );
    post(s, "/strata/jobs", install()).await;
    job_until(s, "setup running", |_| !s.fake.runs_of("python").is_empty()).await;
}

#[tokio::test]
async fn forced_restart_kills_strata_after_the_wait_limit_when_a_job_does_not_end() {
    let s = setup_with("strata-force-restart-stuck", |c| c.cancel_wait_secs = 1).await;
    stuck_install(&s).await;
    let (code, body) = post(&s, "/strata/kill", json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    // The job still runs; it handles Strata, so no start against it.
    assert_eq!(body, json!({"cancelled_job": "install", "started": false}));
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn forced_sleep_sleeps_after_the_wait_limit_when_a_job_does_not_end() {
    let s = setup_with("strata-force-sleep-stuck", |c| c.cancel_wait_secs = 1).await;
    stuck_install(&s).await;
    let (code, body) = post(&s, "/sleep/force", json!({})).await;
    assert_eq!(code, 200, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(4500)).await;
    assert_eq!(s.fake.suspends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forced_restart_starts_no_strata_against_a_job_begun_during_its_pause() {
    let s = setup_with("strata-force-restart-job", |c| c.stop_settle_ms = 2000).await;
    installable(&s);
    s.fake.command(
        "python",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    let url = s.url.clone();
    let kill = tokio::spawn(async move {
        let resp = client()
            .post(format!("{url}/strata/kill"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        resp.json::<Value>().await.unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (code, _) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202);
    assert_eq!(kill.await.unwrap(), json!({"started": false}));
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
}
