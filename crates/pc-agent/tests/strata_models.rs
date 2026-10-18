//! Jobs on an installed Strata: another model, switching and deleting
//! models, update, benchmark and a new Strata key.

mod common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, strata};

const OLD_KEY: &str = "alter-key-0123456789";
const NEW_KEY: &str = "neuer-key-0123456789abcdef";

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

fn write(path: PathBuf, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn config_json(gguf: &str) -> String {
    json!({"exe": "C:/Strata/engine/strata.exe", "args": ["--native", gguf], "port": 8080, "api_key": OLD_KEY})
        .to_string()
}

/// Strata as setup leaves it with IQ3_XXS (active) and coder-IQ1_M.
fn installed(s: &Setup) {
    let strata = s.agent.cfg.strata_dir.clone();
    let data = s.agent.cfg.strata_data_dir.clone();
    write(strata.join("setup.py"), "");
    write(strata.join(".git").join("HEAD"), "0123456789abcdef\n");
    write(strata.join("strata-iq3_xxs.json"), &config_json("a.gguf"));
    write(strata.join("run-iq3_xxs.bat"), "");
    write(
        strata.join("strata-coder-iq1_m.json"),
        &config_json("b.gguf"),
    );
    write(strata.join("run-coder-iq1_m.bat"), "");
    write(strata.join("strata-coder-iq1_m.log"), "log");
    write(
        data.join("models").join("IQ3_XXS").join("a.gguf"),
        "0123456789",
    );
    write(
        data.join("models").join("coder-IQ1_M").join("b.gguf"),
        "01234",
    );
    write(
        data.join("packs")
            .join("coder-iq1_m")
            .join("tokenizer")
            .join("vocab.json"),
        "abc",
    );
    write(data.join("packs").join("iq3_xxs").join("vocab.json"), "abc");
    write(data.join("mtp").join("rt").join("experts.bin"), "shared");
    s.agent
        .set_managed(Managed {
            strata_start: Some(strata.join("run-iq3_xxs.bat")),
            strata_args: vec![],
            strata_processes: Some(vec![format!("{}/", strata.display())]),
            strata_api_key: Some(OLD_KEY.into()),
        })
        .unwrap();
}

fn model<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("{id} fehlt: {view}"))
}

async fn job(s: &Setup, body: Value) -> Value {
    let (code, answer) = post(s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    finished(s).await
}

// --- #31 another model, switching ---

#[tokio::test]
async fn another_model_leaves_the_active_one_active() {
    let s = setup("models-add").await;
    installed(&s);
    let strata = s.agent.cfg.strata_dir.clone();
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            creates: vec![(strata.join("strata-swift-iq2_xs.json"), "{}".into())],
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(args.contains(&"--setup".to_string()), "{args:?}");
    assert!(args.contains(&OLD_KEY.to_string()), "{args:?}");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["active"], true);
    assert_eq!(model(&view, "swift-IQ2_XS")["active"], false);
}

#[tokio::test]
async fn another_model_can_get_the_experimental_speed_projection() {
    let s = setup("models-add-esp").await;
    installed(&s);
    let strata = s.agent.cfg.strata_dir.clone();
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            creates: vec![(strata.join("strata-q2_0.json"), "{}".into())],
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "add_model", "family": "qwen", "size": "Q2_0", "context": 65536,
            "experimental_speed_projection": true}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(
        args.ends_with(&["--experimental-speed-projection".into(), "on".into()]),
        "{args:?}"
    );
}

#[tokio::test]
async fn another_model_without_the_tick_gets_the_speed_projection_off() {
    let s = setup("models-add-esp-off").await;
    installed(&s);
    let done = job(
        &s,
        json!({"kind": "add_model", "family": "qwen", "size": "Q2_0", "context": 65536}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(
        args.ends_with(&["--experimental-speed-projection".into(), "off".into()]),
        "{args:?}"
    );
}

#[tokio::test]
async fn another_model_of_another_family_refuses_the_speed_projection() {
    let s = setup("models-add-esp-swift").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536,
            "experimental_speed_projection": true}),
    )
    .await;
    assert_eq!(code, 400, "{answer}");
    assert!(s.fake.runs_of("START-HERE.bat").is_empty());
}

#[tokio::test]
async fn activating_waits_for_a_running_request_and_switches_the_start_script() {
    let s = setup("models-activate").await;
    installed(&s);
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "activate", "model": "coder-IQ1_M"}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
    job_until(&s, "Warten auf die Anfrage", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("Anfrage"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        s.fake.stops.load(Ordering::SeqCst),
        0,
        "stopped mid-request"
    );

    s.fake.strata_busy.store(false, Ordering::SeqCst);
    assert_eq!(finished(&s).await["state"], "done");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    let run = s.agent.cfg.strata_dir.join("run-coder-iq1_m.bat");
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, run);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "coder-IQ1_M")["active"], true);
    assert_eq!(model(&view, "IQ3_XXS")["active"], false);
    // Also after a restart of the service.
    assert_eq!(s.restarted().managed().strata_start, Some(run));
}

#[tokio::test]
async fn activating_the_active_or_a_missing_model_is_refused() {
    let s = setup("models-activate-refused").await;
    installed(&s);
    for (model, code) in [
        ("IQ3_XXS", 409),
        ("swift-IQ2_XS", 409),
        ("../../Windows", 400),
    ] {
        let (got, answer) = post(
            &s,
            "/strata/jobs",
            json!({"kind": "activate", "model": model}),
        )
        .await;
        assert_eq!(got, code, "{model}: {answer}");
    }
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

// --- #32 deleting ---

#[tokio::test]
async fn deleting_the_active_model_is_refused() {
    let s = setup("models-delete-active").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "delete", "model": "IQ3_XXS"}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
    assert!(s.agent.cfg.strata_data_dir.join("models/IQ3_XXS").is_dir());
}

#[tokio::test]
async fn deleting_removes_exactly_that_models_files() {
    let s = setup("models-delete").await;
    installed(&s);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "coder-IQ1_M")["disk_bytes"], 8, "model + pack");

    let done = job(&s, json!({"kind": "delete", "model": "coder-IQ1_M"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let strata = &s.agent.cfg.strata_dir;
    let data = &s.agent.cfg.strata_data_dir;
    for gone in [
        data.join("models").join("coder-IQ1_M"),
        data.join("packs").join("coder-iq1_m"),
        strata.join("strata-coder-iq1_m.json"),
        strata.join("run-coder-iq1_m.bat"),
        strata.join("strata-coder-iq1_m.log"),
    ] {
        assert!(!gone.exists(), "{} ist noch da", gone.display());
    }
    for kept in [
        data.join("models").join("IQ3_XXS").join("a.gguf"),
        data.join("packs").join("iq3_xxs"),
        data.join("mtp").join("rt").join("experts.bin"),
        strata.join("strata-iq3_xxs.json"),
        strata.join("run-iq3_xxs.bat"),
    ] {
        assert!(kept.exists(), "{} fehlt", kept.display());
    }
    let view = get(&s, "/strata").await;
    assert_eq!(view["models"].as_array().unwrap().len(), 1, "{view}");
}

// --- #33 update ---

#[tokio::test]
async fn update_pulls_runs_setups_update_and_starts_strata() {
    let s = setup("models-update").await;
    installed(&s);
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let strata = s.agent.cfg.strata_dir.clone();
    let pull = s
        .fake
        .runs_of("git")
        .into_iter()
        .find(|c| c.args.first().is_some_and(|a| a == "pull"))
        .expect("git pull");
    assert_eq!(pull.args, ["pull", "--ff-only"]);
    assert_eq!(pull.dir.as_ref(), Some(&strata));
    let update = &s.fake.runs_of("START-HERE.bat")[0];
    assert_eq!(update.args, ["--update"]);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    let (program, _) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, strata.join("run-iq3_xxs.bat"));
}

#[tokio::test]
async fn a_failed_update_starts_strata_again() {
    let s = setup("models-update-failed").await;
    installed(&s);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: lines(&["[X] the engine could not be replaced"]),
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    let done = job(&s, json!({"kind": "update"})).await;
    assert_eq!(done["state"], "failed");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("the engine could not be replaced")
    );
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_page_shows_whether_an_update_is_available() {
    let s = setup("models-update-check").await;
    installed(&s);
    s.fake.command(
        "git",
        FakeCommand {
            lines: lines(&["3"]),
            ..FakeCommand::default()
        },
    );
    let mut view = get(&s, "/strata").await;
    for _ in 0..100 {
        if view["update_available"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        view = get(&s, "/strata").await;
    }
    assert_eq!(view["update_available"], true, "{view}");
    let git = s.fake.runs_of("git");
    assert!(
        git.iter().any(|c| c.args == ["fetch", "--quiet"]),
        "{git:?}"
    );
    assert!(
        git.iter()
            .any(|c| c.args == ["rev-list", "--count", "HEAD..@{u}"]),
        "{git:?}"
    );
    // Asked again only after a while.
    let fetches = |s: &Setup| {
        s.fake
            .runs_of("git")
            .iter()
            .filter(|c| c.args.first().is_some_and(|a| a == "fetch"))
            .count()
    };
    get(&s, "/strata").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fetches(&s), 1);

    // After an update it is current.
    assert_eq!(job(&s, json!({"kind": "update"})).await["state"], "done");
    assert_eq!(get(&s, "/strata").await["update_available"], false);
}

// --- #34 benchmark ---

fn bench(s: &Setup, rows: &[&str]) {
    let mut out = vec![
        "Using the Strata server already running at http://127.0.0.1:8080.",
        "Running the suite (about 3-10 minutes) ...",
        "",
        "Results (median, range over the runs):",
    ];
    out.extend(rows);
    s.fake.command(
        "BENCH.bat",
        FakeCommand {
            lines: lines(&out),
            ..FakeCommand::default()
        },
    );
}

#[tokio::test]
async fn benchmark_keeps_its_result_with_model_and_date() {
    let s = setup("models-bench").await;
    installed(&s);
    bench(
        &s,
        &[
            "  chat decode tok/s                     38.50   (37.10 - 39.20, n=3)",
            "  chat first token (s)                   0.42   (0.40 - 0.45, n=3)",
            "  4K prompt tok/s                      612.00   (600.00 - 620.00, n=3)",
        ],
    );
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["model"], "IQ3_XXS");
    let run = &s.fake.runs_of("BENCH.bat")[0];
    let config = s.agent.cfg.strata_dir.join("strata-iq3_xxs.json");
    assert_eq!(run.args, ["--config", &config.display().to_string()]);

    let view = get(&s, "/strata").await;
    let result = &view["benchmark"];
    assert_eq!(result["model"], "IQ3_XXS");
    assert_eq!(result["result"], "38.5 Token/s");
    assert!(result["at"].as_u64().unwrap() > 1_700_000_000);
    // Also after a restart of the service.
    let again = strata::view(&s.restarted());
    assert_eq!(
        serde_json::to_value(again.benchmark).unwrap()["result"],
        "38.5 Token/s"
    );
}

#[tokio::test]
async fn benchmark_starts_strata_when_it_is_not_running() {
    let s = setup("models-bench-start").await;
    installed(&s);
    s.fake.strata_up.store(false, Ordering::SeqCst);
    bench(&s, &["  chat decode tok/s   20.00   (19.00 - 21.00, n=3)"]);
    assert_eq!(job(&s, json!({"kind": "benchmark"})).await["state"], "done");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn benchmark_without_a_result_fails_and_keeps_the_last_one() {
    let s = setup("models-bench-none").await;
    installed(&s);
    bench(&s, &["  chat decode tok/s   20.00   (19.00 - 21.00, n=3)"]);
    job(&s, json!({"kind": "benchmark"})).await;
    bench(&s, &[]);
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(
        get(&s, "/strata").await["benchmark"]["result"],
        "20 Token/s"
    );
}

// --- #35 new Strata key ---

#[tokio::test]
async fn a_new_key_goes_into_every_model_and_strata_restarts() {
    let s = setup("models-key").await;
    installed(&s);
    let done = job(&s, json!({"kind": "set_key", "api_key": NEW_KEY})).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(!done.to_string().contains(NEW_KEY));
    for name in ["strata-iq3_xxs.json", "strata-coder-iq1_m.json"] {
        let text = std::fs::read_to_string(s.agent.cfg.strata_dir.join(name)).unwrap();
        let cfg: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(cfg["api_key"], NEW_KEY, "{name}");
        assert_eq!(cfg["port"], 8080, "{name}: other values stay");
        assert!(cfg["args"].is_array(), "{name}");
    }
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    get(&s, "/status").await;
    assert_eq!(
        s.fake.health_auth.lock().unwrap().as_deref(),
        Some(format!("Bearer {NEW_KEY}").as_str())
    );
    assert!(!get(&s, "/strata").await.to_string().contains(NEW_KEY));
}

#[tokio::test]
async fn a_bad_key_is_refused() {
    let s = setup("models-key-bad").await;
    installed(&s);
    let (code, _) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_key", "api_key": "kurz"}),
    )
    .await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn jobs_on_strata_need_an_installed_strata() {
    let s = setup("models-not-installed").await;
    for body in [
        json!({"kind": "update"}),
        json!({"kind": "benchmark"}),
        json!({"kind": "set_key", "api_key": NEW_KEY}),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
        assert_eq!(code, 409, "{body}: {answer}");
    }
}

// --- found in review ---

/// Strata set up by hand: start script and key only in the TOML.
async fn set_up_by_hand(name: &str) -> Setup {
    let s = setup_with(name, |c| {
        c.strata_start = Some(c.strata_dir.join("run-iq3_xxs.bat"));
        c.strata_api_key = Some(OLD_KEY.into());
    })
    .await;
    installed(&s);
    s.agent.set_managed(Managed::default()).unwrap();
    s
}

#[tokio::test]
async fn the_model_started_by_the_configuration_counts_as_active() {
    let s = set_up_by_hand("models-by-hand").await;
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "delete", "model": "IQ3_XXS"}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
    assert_eq!(model(&get(&s, "/strata").await, "IQ3_XXS")["active"], true);
}

#[tokio::test]
async fn another_model_gets_the_key_from_the_configuration() {
    let s = set_up_by_hand("models-by-hand-key").await;
    job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(args.contains(&OLD_KEY.to_string()), "{args:?}");
}

#[tokio::test]
async fn the_key_never_shows_in_the_output() {
    let s = setup("models-key-output").await;
    installed(&s);
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: vec![format!("  server 0.0.0.0:8080, api key {OLD_KEY}")],
            exit: Some(1),
            ..FakeCommand::default()
        },
    );
    let done = job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    assert!(!done.to_string().contains(OLD_KEY), "{done}");
    let saved = std::fs::read_to_string(s.dir.join("strata-job.json")).unwrap();
    assert!(!saved.contains(OLD_KEY), "{saved}");
}

#[tokio::test]
async fn the_benchmark_result_is_found_before_much_more_output() {
    let s = setup("models-bench-long").await;
    installed(&s);
    let mut rows = vec!["  chat decode tok/s   42.00   (41.00 - 43.00, n=3)".to_string()];
    rows.extend((0..40).map(|i| format!("  more output {i}")));
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    bench(&s, &rows);
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["result"], "42 Token/s", "{done}");
}

#[tokio::test]
async fn a_damaged_model_config_stops_the_key_change() {
    let s = setup("models-key-damaged").await;
    installed(&s);
    write(
        s.agent.cfg.strata_dir.join("strata-coder-iq1_m.json"),
        "{\"exe\": \"x\",",
    );
    let done = job(&s, json!({"kind": "set_key", "api_key": NEW_KEY})).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(s.agent.managed().strata_api_key.as_deref(), Some(OLD_KEY));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn with_strata_started_by_start_here_the_model_used_last_is_active() {
    let s = setup_with("models-start-here", |c| {
        c.strata_start = Some(c.strata_dir.join("START-HERE.bat"));
    })
    .await;
    installed(&s);
    s.agent.set_managed(Managed::default()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    write(
        s.agent.cfg.strata_dir.join("strata-iq3_xxs.json"),
        &config_json("a.gguf"),
    );
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "delete", "model": "IQ3_XXS"}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
    let done = job(&s, json!({"kind": "benchmark"})).await;
    assert_eq!(done["model"], "IQ3_XXS");
}

#[tokio::test]
async fn another_model_gets_the_key_the_proxy_sends() {
    let s = setup("models-add-key").await;
    installed(&s);
    job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536, "api_key": NEW_KEY}),
    )
    .await;
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(args.contains(&NEW_KEY.to_string()), "{args:?}");
    assert!(!args.contains(&OLD_KEY.to_string()), "{args:?}");
}

#[tokio::test]
async fn a_little_of_a_download_does_not_count_as_most_of_it() {
    let s = setup("models-space-partial").await;
    installed(&s);
    write(
        s.agent
            .cfg
            .strata_data_dir
            .join("models/swift-IQ3_XXS/part-00001.gguf.part"),
        "0123456789",
    );
    *s.fake.disk_free.lock().unwrap() = Some(20_000_000_000);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "add_model", "family": "swift", "size": "IQ3_XXS", "context": 65536}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
}

#[tokio::test]
async fn adding_a_model_needs_an_installed_strata() {
    let s = setup("models-add-not-installed").await;
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536}),
    )
    .await;
    assert_eq!(code, 409, "{answer}");
}

/// After the one-line setup no start script is set: no model counts as
/// active, and activating one, also the only one, sets it and starts Strata.
#[tokio::test]
async fn without_a_start_script_any_installed_model_can_be_activated() {
    let s = setup_with("models-no-start", |c| c.strata_start = None).await;
    installed(&s);
    s.agent.set_managed(Managed::default()).unwrap();
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["active"], false);
    assert_eq!(model(&view, "coder-IQ1_M")["active"], false);
    let done = job(&s, json!({"kind": "activate", "model": "IQ3_XXS"})).await;
    assert_eq!(done["state"], "done", "{done}");
    let managed = s.agent.managed();
    assert_eq!(
        managed.strata_start.as_deref(),
        Some(s.agent.cfg.strata_dir.join("run-iq3_xxs.bat").as_path())
    );
    assert!(managed.strata_processes.is_some());
    assert_eq!(model(&get(&s, "/strata").await, "IQ3_XXS")["active"], true);
    assert!(s.fake.starts.load(Ordering::SeqCst) >= 1);
}

// --- #76 the speed projection of installed models ---

const ESP_FLAGS: [&str; 7] = [
    "--control-vector-layer-range",
    "4",
    "44",
    "--cvec-mode",
    "project",
    "--cvec-dir",
    "per-layer",
];

/// `strata-<id>.json` with these engine args.
fn config_with_args(s: &Setup, id: &str, args: Value) {
    write(
        s.agent.cfg.strata_dir.join(format!("strata-{id}.json")),
        &json!({"exe": "C:/Strata/engine/strata.exe", "args": args, "port": 8080, "api_key": OLD_KEY})
            .to_string(),
    );
}

#[tokio::test]
async fn the_page_shows_whether_a_model_has_the_speed_projection() {
    let s = setup("models-esp-view").await;
    installed(&s);
    let mut args = vec![
        json!("--native"),
        json!("b.gguf"),
        json!("--control-vector-scaled"),
    ];
    args.push(json!("D:/eigen/vektor.gguf:1.0"));
    args.extend(ESP_FLAGS.map(|f| json!(f)));
    config_with_args(&s, "coder-iq1_m", json!(args));
    let view = get(&s, "/strata").await;
    assert_eq!(
        model(&view, "coder-IQ1_M")["speed_projection"],
        true,
        "{view}"
    );
    assert_eq!(model(&view, "IQ3_XXS")["speed_projection"], false, "{view}");
}
