//! Strata management through the service's HTTP interface, with a fake
//! install script, a fake Strata Terminal and a fake setup.

use crate::common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::terminal::Terminal;

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// What `strata update --output jsonl` prints last for the engine's
/// `status` (`strata_cli/update_cmd.py`, `run`, Strata 011c9e6).
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

/// An install on a PC without Strata: the Terminal comes first, then its
/// catalog checks the model; setup never runs for a model it lacks.
async fn install_failing(name: &str, body: Value, why: &str) {
    let s = setup(name).await;
    installable(&s);
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(job["error"].as_str().unwrap().contains(why), "{job}");
    assert!(Terminal::of(&s.agent.cfg).is_installed());
    assert_eq!(s.fake.runs_of("python catalog").len(), 1);
    assert!(s.fake.runs_of("python").is_empty(), "no setup");
}

#[tokio::test]
async fn an_install_checks_the_model_against_the_new_strata_s_catalog() {
    install_failing(
        "strata-catalog-size",
        json!({"kind": "install", "family": "qwen", "size": "XXL", "context": 65536, "api_key": KEY}),
        "qwen XXL is not in Strata's catalog",
    )
    .await;
    install_failing(
        "strata-catalog-context",
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 12345, "api_key": KEY}),
        "There is no context length 12345",
    )
    .await;
}

#[tokio::test]
async fn the_speed_projection_is_refused_for_other_families() {
    install_failing(
        "strata-esp-swift",
        json!({"kind": "install", "family": "swift", "size": "IQ2_XS", "context": 65536,
            "api_key": KEY, "experimental_speed_projection": true}),
        "not for swift-IQ2_XS",
    )
    .await;
}

#[tokio::test]
async fn a_download_that_does_not_fit_is_refused() {
    let s = setup("strata-space").await;
    installable(&s);
    *s.fake.disk_free.lock().unwrap() = Some(50_000_000_000);
    post(&s, "/strata/jobs", install()).await;
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(
        job["error"].as_str().unwrap().contains("Not enough space"),
        "{job}"
    );
    assert!(s.fake.runs_of("python").is_empty(), "no setup");
}

#[tokio::test]
async fn free_values_are_refused_before_anything_runs() {
    let s = setup("strata-free-args").await;
    let body = json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 65536,
        "api_key": KEY, "args": "--evil"});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 422, "{answer}");
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

/// Install as the proxy sends it: Strata alone.
fn strata_alone() -> Value {
    json!({"kind": "install", "api_key": KEY})
}

/// A PC on which Install put Strata alone.
async fn strata_alone_installed(name: &str) -> Setup {
    let s = setup(name).await;
    installable(&s);
    let (code, job) = post(&s, "/strata/jobs", strata_alone()).await;
    assert_eq!(code, 202, "{job}");
    assert!(job.get("model").is_none(), "{job}");
    let job = finished(&s).await;
    assert_eq!(job["state"], "done", "{job}");
    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], true, "{view}");
    s
}

#[tokio::test]
async fn install_without_a_model_puts_only_strata_on_the_pc() {
    let s = strata_alone_installed("strata-alone").await;
    assert!(Terminal::of(&s.agent.cfg).is_installed());
    let engine = s.fake.runs_of("strata.cmd");
    assert_eq!(
        engine[0].args[..2],
        ["update", "--no-control"],
        "the engine"
    );
    assert_eq!(s.fake.runs_of("python catalog").len(), 1);
    assert!(
        s.fake.started.lock().unwrap().is_none(),
        "no model to serve"
    );
    let view = get(&s, "/strata").await;
    assert_eq!(view["models"], json!([]));
    assert_eq!(view["recommended"], "IQ3_S");
    assert!(
        view["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .any(|size| size["id"] == "IQ3_XXS"),
        "{view}"
    );
}

#[tokio::test]
async fn the_first_added_model_is_the_one_strata_serves() {
    let s = strata_alone_installed("strata-first-model").await;
    let add = json!({"kind": "add_model", "family": "qwen", "size": "IQ3_XXS", "context": 65536});
    let (code, answer) = post(&s, "/strata/jobs", add).await;
    assert_eq!(code, 202, "{answer}");
    assert_eq!(finished(&s).await["state"], "done");
    let cfg = &s.agent.cfg;
    let terminal = cfg.strata_home.join("terminal").join("current");
    let config = cfg.strata_home.join("install").join("strata-iq3_xxs.json");
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, terminal.join("bin").join("strata.cmd"));
    assert_eq!(
        args[..3],
        ["serve", "--profile", &config.display().to_string()]
    );
    let view = get(&s, "/strata").await;
    assert_eq!(view["models"][0]["id"], "IQ3_XXS");
    assert_eq!(view["models"][0]["active"], true);
}

#[tokio::test]
async fn an_unreadable_catalog_leaves_strata_installed_and_says_why() {
    let s = setup("strata-alone-unreadable").await;
    installable(&s);
    s.fake.command(
        "python catalog",
        FakeCommand {
            lines: lines(&["Traceback (most recent call last):"]),
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", strata_alone()).await;
    assert_eq!(finished(&s).await["state"], "done");
    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], true);
    assert!(view["catalog_unreadable"].is_string(), "{view}");
    let add = json!({"kind": "add_model", "family": "qwen", "size": "IQ3_XXS", "context": 65536});
    let (code, answer) = post(&s, "/strata/jobs", add).await;
    assert_eq!(code, 409, "{answer}");
    assert!(
        answer
            .to_string()
            .contains("update Strata to choose a model"),
        "{answer}"
    );
}

#[tokio::test]
async fn an_install_takes_a_whole_model_or_none() {
    let s = setup("strata-half-model").await;
    let half = json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "api_key": KEY});
    let (code, answer) = post(&s, "/strata/jobs", half).await;
    assert_eq!(code, 400, "{answer}");
    assert!(
        answer
            .to_string()
            .contains("family, size and context together, or none of them"),
        "{answer}"
    );
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_install_without_a_model_takes_no_model_ticks() {
    let s = setup("strata-alone-ticks").await;
    let ticks = json!({"kind": "install", "api_key": KEY, "pictures": true});
    let (code, answer) = post(&s, "/strata/jobs", ticks).await;
    assert_eq!(code, 400, "{answer}");
    assert!(
        answer
            .to_string()
            .contains("Without a model there is no speed projection or picture reading"),
        "{answer}"
    );
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
    assert_eq!(update.args, ["update", "--no-control", "--output", "jsonl"]);
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
async fn install_sets_up_picture_reading_when_asked_for_it() {
    for (pictures, value) in [(true, "yes"), (false, "no")] {
        let s = setup(&format!("strata-pictures-{value}")).await;
        installable(&s);
        let mut body = install();
        body["pictures"] = json!(pictures);
        let (code, job) = post(&s, "/strata/jobs", body).await;
        assert_eq!(code, 202, "{job}");
        assert_eq!(finished(&s).await["state"], "done");
        let args = &s.fake.runs_of("python")[0].args;
        assert!(
            args.windows(2).any(|w| w == ["--vision", value]),
            "{value}: {args:?}"
        );
    }
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

#[tokio::test]
async fn the_page_says_the_vram_it_counts() {
    let s = setup("strata-vram").await;
    pc_with(&s, 32.0, 16.0);
    assert_eq!(get(&s, "/strata").await["vram_gb"], 16.0);
    pc_with(&s, 32.0, 0.0);
    assert_eq!(get(&s, "/strata").await["vram_gb"], Value::Null);
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

/// Events `strata update --output jsonl` prints on its way, as `emit` in
/// `strata_control/updater.py` writes them (Strata 5c8dd32).
const FEED: &str = r#"{"schema_version": 1, "event": "phase", "phase": "feed", "message": "Reading the signed update feed"}"#;
const DOWNLOAD: &str = r#"{"schema_version": 1, "event": "phase", "phase": "download", "message": "Downloading engine 0.1.45 windows-x86_64-cuda (412 MiB)", "component": "engine"}"#;
const DOWNLOADED: &str = r#"{"schema_version": 1, "event": "progress", "phase": "download", "component": "engine", "label": "engine 0.1.45 windows-x86_64-cuda", "done": 104857600, "total": 432013312, "percent": 24, "bytes_per_second": 20971520}"#;
const UNPACKING: &str = r#"{"schema_version": 1, "event": "phase", "phase": "verify", "message": "Unpacking 0.1.45 windows-x86_64-cuda", "component": "engine"}"#;

#[tokio::test]
async fn the_engine_install_shows_strata_s_phases_and_download() {
    let s = setup("strata-engine-progress").await;
    installable(&s);
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: lines(&[FEED, DOWNLOAD, DOWNLOADED]),
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = job_until(&s, "engine download", |j| j["percent"] == 24).await;
    assert_eq!(
        job["step"],
        "Downloading engine 0.1.45 windows-x86_64-cuda (412 MiB)"
    );
    assert_eq!(job["done_bytes"], 104857600);
    assert_eq!(job["total_bytes"], 432013312);
    assert_eq!(job["bytes_per_sec"], 20971520);
    // (432013312 - 104857600) / 20971520
    assert_eq!(job["eta_secs"], 15);
    let shown: Vec<&str> = job["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(shown.contains(&"Reading the signed update feed"), "{job}");
    assert!(!shown.iter().any(|l| l.contains('{')), "{job}");
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    assert_eq!(finished(&s).await["state"], "cancelled");
}

#[tokio::test]
async fn the_engine_install_ends_with_strata_s_reason_after_its_phases() {
    let s = setup("strata-engine-phases").await;
    installable(&s);
    let refused = r#"{"schema_version": 1, "ok": false, "error": {"code": 5, "message": "engine 0.1.45: the signature does not match"}}"#;
    s.fake.command(
        "strata.cmd update",
        FakeCommand {
            lines: lines(&[DOWNLOAD, DOWNLOADED, UNPACKING, refused]),
            exit: Some(5),
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    let job = finished(&s).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(job["error"], "engine 0.1.45: the signature does not match");
    // The last phase came without the download's figures.
    assert_eq!(job["step"], "Unpacking 0.1.45 windows-x86_64-cuda");
    for figure in [
        "percent",
        "done_bytes",
        "total_bytes",
        "bytes_per_sec",
        "eta_secs",
    ] {
        assert!(job[figure].is_null(), "{figure}: {job}");
    }
    // After the install script's lines: the phases, no JSON.
    let shown = job["lines"].as_array().unwrap();
    assert_eq!(
        shown[shown.len() - 2..],
        [
            "Downloading engine 0.1.45 windows-x86_64-cuda (412 MiB)",
            "Unpacking 0.1.45 windows-x86_64-cuda"
        ]
    );
    assert!(
        !shown.iter().any(|l| l.as_str().unwrap().contains('{')),
        "{job}"
    );
    assert!(s.fake.runs_of("python").is_empty(), "no model download");
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
    // Without Strata the model is checked only once the Terminal is there.
    assert_eq!(job["model"], Value::Null);
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
