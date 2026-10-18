//! Strata management through the service's HTTP interface, with fake git,
//! winget and setup.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::{Value, json};

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

/// Git clones a Strata folder; setup sets up IQ3_XXS.
fn installable(s: &Setup) {
    let strata = s.agent.cfg.strata_dir.clone();
    let data = s.agent.cfg.strata_data_dir.clone();
    s.fake.command(
        "git",
        FakeCommand {
            creates: vec![
                (strata.join("setup.py"), String::new()),
                (strata.join("START-HERE.bat"), String::new()),
                (
                    strata.join(".git").join("HEAD"),
                    "ref: refs/heads/main\n".into(),
                ),
                (
                    strata.join(".git").join("refs").join("heads").join("main"),
                    "477ac13ffffffffffffffffffffffffffffffff\n".into(),
                ),
            ],
            ..FakeCommand::default()
        },
    );
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: lines(&[
                "=== Step 1: Checking this PC ===",
                "=== Step 4: Downloading the model ===",
                "  Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf: 37.9 / 75.8 GB (50%)",
            ]),
            creates: vec![
                (strata.join("strata-iq3_xxs.json"), "{}".into()),
                (strata.join("run-iq3_xxs.bat"), String::new()),
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

#[tokio::test]
async fn values_outside_the_catalog_are_refused() {
    let s = setup("strata-catalog").await;
    for body in [
        json!({"kind": "install", "family": "qwen", "size": "XXL", "context": 65536, "api_key": KEY}),
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 12345, "api_key": KEY}),
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 65536, "api_key": "a b"}),
        json!({"kind": "install", "family": "qwen", "size": "IQ3_XXS", "context": 65536, "api_key": KEY, "args": "--evil"}),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
        assert!(code == 400 || code == 422, "{body}: {code} {answer}");
    }
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn install_runs_setup_with_the_fixed_values_and_starts_strata() {
    let s = setup("strata-install").await;
    installable(&s);
    let (code, job) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 202, "{job}");
    assert!(!job.to_string().contains(KEY), "{job}");
    let job = finished(&s).await;
    assert_eq!(job["state"], "done", "{job}");

    let clone = &s.fake.runs_of("git")[1];
    assert_eq!(
        clone.args,
        [
            "clone",
            "https://example.org/Strata.git",
            &s.agent.cfg.strata_dir.display().to_string()
        ]
    );
    let setup = &s.fake.runs_of("START-HERE.bat")[0];
    let data = s.agent.cfg.strata_data_dir.display().to_string();
    assert_eq!(
        setup.args,
        [
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
            "--api-key",
            KEY,
            "--experimental-speed-projection",
            "off",
        ]
    );

    // From now on the service starts the new model with the new key.
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, s.agent.cfg.strata_dir.join("run-iq3_xxs.bat"));
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped, ["llama-server.exe"], "the old Strata ends first");
    get(&s, "/status").await;
    assert_eq!(
        s.fake.health_auth.lock().unwrap().as_deref(),
        Some(format!("Bearer {KEY}").as_str())
    );
    s.agent.set_gaming(true);
    let stopped = s.fake.stopped.lock().unwrap().clone().unwrap();
    assert_eq!(stopped.len(), 1);
    assert!(stopped[0].starts_with(&s.agent.cfg.strata_dir.display().to_string()));

    let view = get(&s, "/strata").await;
    assert_eq!(view["installed"], true);
    assert_eq!(view["version"], "477ac13");
    assert_eq!(view["models"][0]["id"], "IQ3_XXS");
    assert_eq!(view["models"][0]["active"], true);
    assert_eq!(view["models"][0]["disk_bytes"], 10);
    assert!(!view.to_string().contains(KEY));
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
        let args = &s.fake.runs_of("START-HERE.bat")[0].args;
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
    assert!(answer.to_string().contains("Qwen und Coder"), "{answer}");
    assert!(s.fake.ran.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_git_is_installed_with_winget() {
    let s = setup("strata-git").await;
    installable(&s);
    s.fake.git_missing.store(true, Ordering::SeqCst);
    post(&s, "/strata/jobs", install()).await;
    assert_eq!(finished(&s).await["state"], "done");
    let winget = s.fake.runs_of("winget");
    assert_eq!(winget.len(), 1);
    assert!(winget[0].args.contains(&"Git.Git".to_string()));
    assert!(s.fake.runs_of("git").iter().any(|c| c.args[0] == "clone"));
}

#[tokio::test]
async fn a_second_job_is_refused_while_one_runs() {
    let s = setup("strata-second").await;
    installable(&s);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    assert_eq!(post(&s, "/strata/jobs", install()).await.0, 202);
    job_until(&s, "Setup läuft", |_| {
        !s.fake.runs_of("START-HERE.bat").is_empty()
    })
    .await;
    let (code, answer) = post(&s, "/strata/jobs", install()).await;
    assert_eq!(code, 409);
    assert!(
        answer["reason"].as_str().unwrap().contains("läuft schon"),
        "{answer}"
    );
}

#[tokio::test]
async fn a_running_job_keeps_the_pc_awake_and_the_watchdog_away() {
    let s = setup("strata-awake").await;
    installable(&s);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "Setup läuft", |_| {
        !s.fake.runs_of("START-HERE.bat").is_empty()
    })
    .await;

    let (code, answer) = sleep_answer(&s).await;
    assert_eq!(code, 409);
    assert_eq!(answer["reason"], "Strata-Einrichtung läuft");
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
            .contains("Zu wenig Platz"),
        "{answer}"
    );
}

#[tokio::test]
async fn a_failed_setup_shows_its_error_and_last_lines() {
    let s = setup("strata-fail").await;
    installable(&s);
    s.fake.command(
        "START-HERE.bat",
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
    assert_eq!(job["lines"].as_array().unwrap().len(), 3);
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
    assert_eq!(view["recommended"], "IQ3_XXS");
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

// --- Progress, cancel, resume (#30) ---

#[tokio::test]
async fn progress_comes_from_setups_output() {
    let s = setup("strata-progress").await;
    installable(&s);
    s.fake.command(
        "START-HERE.bat",
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
    let job = job_until(&s, "Downloadstand", |j| j["percent"] == 12).await;
    assert_eq!(job["step"], "4/7: Downloading the model");
    assert_eq!(job["done_bytes"], 10_000_000_000u64);
    assert_eq!(job["total_bytes"], 80_000_000_000u64);
}

#[tokio::test]
async fn cancel_ends_the_running_command() {
    let s = setup("strata-cancel").await;
    installable(&s);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "Setup läuft", |_| {
        !s.fake.runs_of("START-HERE.bat").is_empty()
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
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "Setup läuft", |_| {
        !s.fake.runs_of("START-HERE.bat").is_empty()
    })
    .await;
    let again = s.restarted();
    let job = again.status().await;
    assert!(!job.strata_job);
    let addr = serve(router(again)).await;
    let job: Value = client()
        .get(format!("http://{addr}/strata/job"))
        .bearer_auth("geheim")
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
        "START-HERE.bat",
        FakeCommand {
            hang: true,
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", install()).await;
    job_until(&s, "Setup läuft", |_| {
        !s.fake.runs_of("START-HERE.bat").is_empty()
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
