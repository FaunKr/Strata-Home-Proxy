//! Jobs on an installed Strata: another model, switching and deleting
//! models, update, benchmark and a new Strata key.

mod common;

use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

use common::*;
use serde_json::{Value, json};
use strata_pc_agent::{install::testing::SetupLeaves, managed::Managed, strata};

const OLD_KEY: &str = "old-key-0123456789";
const NEW_KEY: &str = "neuer-key-0123456789abcdef";

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

fn write(path: PathBuf, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A config as the service leaves it after its start.
fn config_json(gguf: &str) -> Value {
    json!({"exe": "C:/Strata/engine/strata.exe", "args": ["--native", gguf], "port": 8080,
        "api_key": OLD_KEY, "reasoning_loop_recovery": "recover"})
}

/// Strata as setup leaves it with IQ3_XXS (active) and coder-IQ1_M.
fn installed(s: &Setup) {
    let cfg = &s.agent.cfg;
    let setup = SetupLeaves::new(&cfg.strata_dir, &cfg.strata_data_dir, "0123456789abcdef");
    setup
        .model("iq3_xxs", config_json("a.gguf"))
        .model("coder-iq1_m", config_json("b.gguf"))
        .log("coder-iq1_m", "log")
        .download("IQ3_XXS", "a.gguf", "0123456789")
        .download("coder-IQ1_M", "b.gguf", "01234")
        .pack("coder-iq1_m", "tokenizer/vocab.json", "abc")
        .pack("iq3_xxs", "vocab.json", "abc")
        .shared("mtp/rt/experts.bin", "shared");
    s.agent
        .set_managed(Managed {
            strata_start: Some(setup.installation().run_script("iq3_xxs")),
            strata_args: vec![],
            strata_processes: Some(vec![format!("{}/", cfg.strata_dir.display())]),
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
    job_until(&s, "waiting for the request", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("request"))
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
        assert!(!gone.exists(), "{} is still there", gone.display());
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

// --- #142 incoming changes ---

/// Git with two commits upstream: one with a body of two lines.
fn upstream_git(s: &Setup) {
    let fake = |out: &[&str]| FakeCommand {
        lines: lines(out),
        ..FakeCommand::default()
    };
    s.fake.command("git rev-list", fake(&["2"]));
    s.fake.command("git rev-parse", fake(&["fedcba9876543210"]));
    s.fake.command(
        "git log",
        fake(&[
            "fedcba9876543210\u{1f}1760000000\u{1f}Faster prefill\u{1f}Reads the prompt in bigger chunks.",
            "Needs setup again.\u{1e}",
            "aaaabbbbccccdddd\u{1f}1759990000\u{1f}Fix a crash\u{1f}\u{1e}",
        ]),
    );
    s.fake.command(
        "git diff",
        fake(&[
            "120\t30\tserve/server.py",
            "-\t-\tdocs/logo.png",
            "4\t0\tsetup.py",
        ]),
    );
    s.fake.command(
        "git ls-remote",
        fake(&["https://github.com/Niko1221/Strata.git"]),
    );
}

async fn view_once_checked(s: &Setup) -> Value {
    let mut view = get(s, "/strata").await;
    for _ in 0..100 {
        if view["update_available"] != Value::Null {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        view = get(s, "/strata").await;
    }
    view
}

#[tokio::test]
async fn the_page_carries_the_incoming_changes() {
    let s = setup("models-update-changes").await;
    installed(&s);
    upstream_git(&s);
    let view = view_once_checked(&s).await;
    assert_eq!(view["update_available"], true, "{view}");
    let update = &view["update"];
    assert_eq!(update["upstream"], "fedcba9876543210", "{view}");
    assert_eq!(update["total"], 2);
    assert_eq!(update["remote"], "https://github.com/Niko1221/Strata.git");
    assert_eq!(
        update["commits"],
        json!([
            {"hash": "fedcba9876543210", "time": 1760000000u64, "subject": "Faster prefill",
             "body": "Reads the prompt in bigger chunks.\nNeeds setup again."},
            {"hash": "aaaabbbbccccdddd", "time": 1759990000u64, "subject": "Fix a crash", "body": ""},
        ])
    );
    assert_eq!(
        update["files"],
        json!([
            {"path": "serve/server.py", "added": 120, "removed": 30},
            {"path": "docs/logo.png", "added": 0, "removed": 0},
            {"path": "setup.py", "added": 4, "removed": 0},
        ])
    );
    let log = s
        .fake
        .runs_of("git")
        .into_iter()
        .find(|c| c.args[0] == "log")
        .unwrap();
    assert!(log.args.contains(&"HEAD..@{u}".to_string()), "{log:?}");
    assert!(log.args.contains(&"200".to_string()), "{log:?}");

    // After an update it is gone.
    assert_eq!(job(&s, json!({"kind": "update"})).await["state"], "done");
    assert_eq!(get(&s, "/strata").await.get("update"), None);
}

#[tokio::test]
async fn nothing_incoming_means_no_changes() {
    let s = setup("models-update-none").await;
    installed(&s);
    upstream_git(&s);
    s.fake.command(
        "git rev-list",
        FakeCommand {
            lines: lines(&["0"]),
            ..FakeCommand::default()
        },
    );
    let view = view_once_checked(&s).await;
    assert_eq!(view["update_available"], false, "{view}");
    assert_eq!(view.get("update"), None);
    assert!(!s.fake.runs_of("git").iter().any(|c| c.args[0] == "log"));
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
        serde_json::to_value(again.info.benchmark).unwrap()["result"],
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

#[tokio::test]
async fn benchmark_keeps_each_models_speeds_and_settings() {
    let s = setup("models-bench-each").await;
    installed(&s);
    bench(
        &s,
        &[
            "  chat decode tok/s   38.50   (37.10 - 39.20, n=3)",
            "  4K prompt tok/s    612.00   (600.00 - 620.00, n=3)",
        ],
    );
    job(&s, json!({"kind": "benchmark"})).await;
    let view = get(&s, "/strata").await;
    let rows = view["benchmarks"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{view}");
    assert_eq!(rows[0]["model"], "IQ3_XXS");
    assert_eq!(rows[0]["decode_tps"], 38.5);
    assert_eq!(rows[0]["prompt_tps"], 612.0);
    assert_eq!(rows[0]["speed_projection"], false);
    assert_eq!(rows[0]["version"], "0123456");
}

#[tokio::test]
async fn an_older_services_benchmark_counts_as_its_models_row() {
    let s = setup("models-bench-old").await;
    installed(&s);
    write(
        s.agent.cfg.state_dir().join("strata-benchmark.json"),
        r#"{"model": "IQ3_XXS", "result": "38.5 Token/s", "at": 1800000000}"#,
    );
    let rows = &get(&s, "/strata").await["benchmarks"];
    assert_eq!(rows[0]["model"], "IQ3_XXS", "{rows}");
    assert_eq!(rows[0]["decode_tps"], 38.5);
}

// --- Comparing models ---

#[tokio::test]
async fn comparing_benchmarks_every_model_and_the_active_one_comes_back() {
    let s = setup("models-compare").await;
    installed(&s);
    bench(&s, &["  chat decode tok/s   20.00   (19.00 - 21.00, n=3)"]);
    let (code, answer) = post(&s, "/strata/jobs", json!({"kind": "compare_models"})).await;
    assert_eq!(code, 202, "{answer}");
    let done = finished_slowly(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["result"], "Fastest: IQ3_XXS with 20 Token/s");
    let dir = &s.agent.cfg.strata_dir;
    let configs: Vec<String> = s
        .fake
        .runs_of("BENCH.bat")
        .iter()
        .map(|r| r.args[1].clone())
        .collect();
    assert_eq!(
        configs,
        [
            dir.join("strata-iq3_xxs.json").display().to_string(),
            dir.join("strata-coder-iq1_m.json").display().to_string(),
        ]
    );
    assert_eq!(
        s.agent.managed().strata_start,
        Some(dir.join("run-iq3_xxs.bat"))
    );
    let view = get(&s, "/strata").await;
    assert_eq!(view["benchmarks"].as_array().unwrap().len(), 2, "{view}");
    assert!(model(&view, "IQ3_XXS")["active"].as_bool().unwrap());
}

#[tokio::test]
async fn a_model_without_a_result_keeps_its_error_and_the_next_follows() {
    let s = setup("models-compare-fail").await;
    installed(&s);
    bench(&s, &[]);
    let (code, _) = post(&s, "/strata/jobs", json!({"kind": "compare_models"})).await;
    assert_eq!(code, 202);
    let done = finished_slowly(&s).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(s.fake.runs_of("BENCH.bat").len(), 2);
    let view = get(&s, "/strata").await;
    for row in view["benchmarks"].as_array().unwrap() {
        assert!(row["error"].as_str().unwrap().contains("tokens/s"), "{row}");
        assert!(row.get("decode_tps").is_none(), "{row}");
    }
}

#[tokio::test]
async fn a_failed_run_keeps_the_models_last_result_next_to_its_error() {
    let s = setup("models-compare-keep").await;
    installed(&s);
    bench(&s, &["  chat decode tok/s   38.50   (37.10 - 39.20, n=3)"]);
    job(&s, json!({"kind": "benchmark"})).await;
    bench(&s, &[]);
    post(&s, "/strata/jobs", json!({"kind": "compare_models"})).await;
    assert_eq!(finished_slowly(&s).await["state"], "done");
    let view = get(&s, "/strata").await;
    let rows = view["benchmarks"].as_array().unwrap();
    let active = rows.iter().find(|r| r["model"] == "IQ3_XXS").unwrap();
    assert_eq!(active["decode_tps"], 38.5, "{active}");
    assert!(
        active["error"].as_str().unwrap().contains("tokens/s"),
        "{active}"
    );
    let other = rows.iter().find(|r| r["model"] != "IQ3_XXS").unwrap();
    assert!(other.get("decode_tps").is_none(), "{other}");
}

#[tokio::test]
async fn cancelling_a_comparison_brings_the_active_model_back() {
    let s = setup("models-compare-cancel").await;
    installed(&s);
    // Each run ends after 2 s without a result.
    s.fake.command(
        "BENCH.bat",
        FakeCommand {
            lines: lines(&["Running the suite ..."]),
            pause: Duration::from_secs(2),
            ..FakeCommand::default()
        },
    );
    post(&s, "/strata/jobs", json!({"kind": "compare_models"})).await;
    // The second run is coder's, with coder active.
    for _ in 0..400 {
        if s.fake.runs_of("BENCH.bat").len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let coder = s.agent.cfg.strata_dir.join("run-coder-iq1_m.bat");
    assert_eq!(s.agent.managed().strata_start, Some(coder));
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    assert_eq!(finished_slowly(&s).await["state"], "cancelled");
    assert_eq!(
        s.agent.managed().strata_start,
        Some(s.agent.cfg.strata_dir.join("run-iq3_xxs.bat"))
    );
}

// --- #35 new Strata key, #219 a new key with every start ---

/// The key every model config holds; panics if two differ.
fn config_key(s: &Setup) -> String {
    let keys: Vec<String> = ["iq3_xxs", "coder-iq1_m"]
        .iter()
        .map(|id| read_config(s, id)["api_key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys[0], keys[1], "every model holds the same key");
    keys[0].clone()
}

fn modified(s: &Setup, id: &str) -> std::time::SystemTime {
    let path = s.agent.cfg.strata_dir.join(format!("strata-{id}.json"));
    std::fs::metadata(path).unwrap().modified().unwrap()
}

async fn key_answer(s: &Setup) -> (u16, Value) {
    let resp = client()
        .get(format!("{}/strata/key", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.json().await.unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn every_start_writes_a_new_key_into_every_model() {
    let s = setup("models-key-start").await;
    installed(&s);
    let generation = get(&s, "/status").await["strata_key_generation"].clone();
    assert!(generation.is_u64(), "the configs' key counts: {generation}");
    let before = modified(&s, "coder-iq1_m");
    std::thread::sleep(Duration::from_millis(20));

    s.agent.strata().start();
    let first = config_key(&s);
    assert_ne!(first, OLD_KEY);
    assert_eq!(first.len(), 64, "{first}");
    assert_eq!(modified(&s, "coder-iq1_m"), before, "the time stays");
    assert_eq!(
        read_config(&s, "iq3_xxs")["port"],
        8080,
        "other values stay"
    );
    let status = get(&s, "/status").await;
    assert_ne!(status["strata_key_generation"], generation);
    assert!(!status.to_string().contains(&first), "{status}");
    assert_eq!(
        s.fake.health_auth.lock().unwrap().as_deref(),
        Some(format!("Bearer {first}").as_str())
    );

    s.agent.strata().restart().await;
    let second = config_key(&s);
    assert_ne!(second, first);
    let (code, answer) = key_answer(&s).await;
    assert_eq!(code, 200);
    assert_eq!(answer["api_key"], second);
    assert_eq!(
        answer["generation"],
        get(&s, "/status").await["strata_key_generation"]
    );
}

#[tokio::test]
async fn before_a_start_the_key_is_the_one_in_the_configs() {
    let s = setup("models-key-configs").await;
    installed(&s);
    let (code, answer) = key_answer(&s).await;
    assert_eq!(code, 200);
    assert_eq!(answer["api_key"], OLD_KEY);
}

#[tokio::test]
async fn without_a_key_in_any_config_there_is_none() {
    let s = setup("models-key-none").await;
    assert_eq!(key_answer(&s).await.0, 404);
    // The generation still tells the proxy this service makes keys.
    assert!(get(&s, "/status").await["strata_key_generation"].is_u64());
    // Without model configs nothing holds a key, and Strata starts anyway.
    s.agent.strata().start();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(key_answer(&s).await.0, 404);
}

#[tokio::test]
async fn the_key_needs_the_token() {
    let s = setup("models-key-token").await;
    installed(&s);
    let resp = client()
        .get(format!("{}/strata/key", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn a_key_that_cannot_be_written_starts_no_strata() {
    let s = setup("models-key-unwritable").await;
    installed(&s);
    // A folder where the temporary file goes makes the write fail.
    let tmp = s.agent.cfg.strata_dir.join("strata-iq3_xxs.json.tmp");
    std::fs::create_dir_all(tmp.join("x")).unwrap();
    s.agent.strata().start();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0);
    assert_eq!(config_key(&s), OLD_KEY, "no config keeps half a change");
}

#[tokio::test]
async fn set_key_restarts_strata_with_a_key_of_its_own() {
    let s = setup("models-key").await;
    installed(&s);
    let done = job(&s, json!({"kind": "set_key", "api_key": NEW_KEY})).await;
    assert_eq!(done["state"], "done", "{done}");
    let key = config_key(&s);
    assert_ne!(key, NEW_KEY, "an older proxy's key is not used");
    assert_ne!(key, OLD_KEY);
    assert!(!done.to_string().contains(&key));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 1);
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert!(!get(&s, "/strata").await.to_string().contains(&key));
}

#[tokio::test]
async fn set_key_takes_any_key_an_older_proxy_sends() {
    let s = setup("models-key-any").await;
    installed(&s);
    let (code, answer) = post(
        &s,
        "/strata/jobs",
        json!({"kind": "set_key", "api_key": "kurz"}),
    )
    .await;
    assert_eq!(code, 202, "{answer}");
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
async fn a_damaged_model_config_keeps_its_text_and_strata_still_starts() {
    let s = setup("models-key-damaged").await;
    installed(&s);
    strata_folder(&s).config_text("coder-iq1_m", "{\"exe\": \"x\",");
    s.agent.strata().start();
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    assert_ne!(read_config(&s, "iq3_xxs")["api_key"], OLD_KEY);
    let damaged = s.agent.cfg.strata_dir.join("strata-coder-iq1_m.json");
    assert_eq!(
        std::fs::read_to_string(damaged).unwrap(),
        "{\"exe\": \"x\","
    );
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
    strata_folder(&s).config("iq3_xxs", config_json("a.gguf"));
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
async fn another_model_gets_the_key_strata_runs_with_not_the_proxys() {
    let s = setup("models-add-key").await;
    installed(&s);
    job(
        &s,
        json!({"kind": "add_model", "family": "swift", "size": "IQ2_XS", "context": 65536, "api_key": NEW_KEY}),
    )
    .await;
    let args = &s.fake.runs_of("START-HERE.bat")[0].args;
    assert!(args.contains(&OLD_KEY.to_string()), "{args:?}");
    assert!(!args.contains(&NEW_KEY.to_string()), "{args:?}");
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

/// A model config with these engine args.
fn config_with_args(s: &Setup, id: &str, args: Value) {
    strata_folder(s).config(
        id,
        json!({"exe": "C:/Strata/engine/strata.exe", "args": args, "port": 8080, "api_key": OLD_KEY}),
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
    args.push(json!("D:/own/vector.gguf:1.0"));
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

/// Strata's own vector, where setup expects it; its path as setup writes it.
fn vector(s: &Setup) -> String {
    let strata = strata_folder(s);
    strata.speed_projection_vector();
    format!("{}:1.0", strata.vector().display())
}

fn args_of(s: &Setup, id: &str) -> Value {
    let cfg = read_config(s, id);
    // A restart brings a new key; the key stays one.
    assert!(cfg["api_key"].is_string(), "other values stay: {cfg}");
    assert_eq!(cfg["port"], 8080, "other values stay: {cfg}");
    cfg["args"].clone()
}

#[tokio::test]
async fn switching_the_speed_projection_on_writes_setups_flags() {
    let s = setup("models-esp-on").await;
    installed(&s);
    let vector = vector(&s);
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "coder-IQ1_M", "on": true}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let mut expected = vec![json!("--native"), json!("b.gguf")];
    expected.extend([json!("--control-vector-scaled"), json!(vector)]);
    expected.extend(ESP_FLAGS.map(|f| json!(f)));
    assert_eq!(args_of(&s, "coder-iq1_m"), json!(expected));
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0, "an inactive model");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0, "an inactive model");
}

#[tokio::test]
async fn switching_the_speed_projection_off_removes_it_whatever_its_values() {
    let s = setup("models-esp-off").await;
    installed(&s);
    let args = json!([
        "--native",
        "b.gguf",
        "--control-vector-scaled",
        "D:/own/vector.gguf:0.5",
        "--cvec-mode",
        "add",
        "--threads",
        "8",
        "--control-vector-layer-range",
        "2",
        "30",
        "--cvec-dir",
        "per-layer"
    ]);
    config_with_args(&s, "coder-iq1_m", args);
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "coder-IQ1_M", "on": false}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        args_of(&s, "coder-iq1_m"),
        json!(["--native", "b.gguf", "--threads", "8"])
    );
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "coder-IQ1_M")["speed_projection"], false);
}

#[tokio::test]
async fn switching_on_twice_keeps_one_set_of_flags() {
    let s = setup("models-esp-twice").await;
    installed(&s);
    vector(&s);
    for _ in 0..2 {
        let body = json!({"kind": "set_speed_projection", "model": "IQ3_XXS", "on": true});
        let (code, answer) = post(&s, "/strata/jobs", body).await;
        assert_eq!(code, 202, "{answer}");
        finished(&s).await;
    }
    let args = args_of(&s, "iq3_xxs");
    let flags = args
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| *a == "--control-vector-scaled");
    assert_eq!(flags.count(), 1, "{args}");
}

#[tokio::test]
async fn switching_the_speed_projection_is_refused_where_it_does_not_fit() {
    let s = setup("models-esp-refused").await;
    installed(&s);
    strata_folder(&s).config("swift-iq2_xs", config_json("c.gguf"));
    for (body, status) in [
        (
            json!({"kind": "set_speed_projection", "model": "swift-IQ2_XS", "on": true}),
            400,
        ),
        (
            json!({"kind": "set_speed_projection", "model": "swift-IQ2_XS", "on": false}),
            400,
        ),
        (
            json!({"kind": "set_speed_projection", "model": "Q2_0", "on": true}),
            409,
        ),
        (
            json!({"kind": "set_speed_projection", "model": "nothing", "on": true}),
            400,
        ),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
        assert_eq!(code, status, "{body}: {answer}");
    }
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let activate = json!({"kind": "activate", "model": "coder-IQ1_M"});
    assert_eq!(post(&s, "/strata/jobs", activate).await.0, 202);
    let body = json!({"kind": "set_speed_projection", "model": "IQ3_XXS", "on": true});
    let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
    assert_eq!(code, 409, "a job running: {answer}");
    s.fake.strata_busy.store(false, Ordering::SeqCst);
    finished(&s).await;
    s.agent.set_gaming(true);
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 409, "Gaming: {answer}");
}

#[tokio::test]
async fn without_strata_s_vector_switching_on_fails_and_changes_nothing() {
    let s = setup("models-esp-no-vector").await;
    installed(&s);
    let before = strata_folder(&s).text("coder-iq1_m");
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "coder-IQ1_M", "on": true}),
    )
    .await;
    assert_eq!(done["state"], "failed", "{done}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .contains("experimental-speed-projection"),
        "{done}"
    );
    let after = strata_folder(&s).text("coder-iq1_m");
    assert_eq!(before, after);
}

#[tokio::test]
async fn switching_the_active_model_restarts_strata_after_the_running_request() {
    let s = setup("models-esp-active").await;
    installed(&s);
    vector(&s);
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let body = json!({"kind": "set_speed_projection", "model": "IQ3_XXS", "on": true});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    job_until(&s, "waiting for the request", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("request"))
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
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["speed_projection"], true);
    assert_eq!(model(&view, "IQ3_XXS")["active"], true);
}

#[tokio::test]
async fn a_failed_switch_of_the_active_model_leaves_strata_running() {
    let s = setup("models-esp-active-failed").await;
    installed(&s);
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "IQ3_XXS", "on": true}),
    )
    .await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn switching_a_model_keeps_the_model_start_here_starts() {
    let s = setup_with("models-esp-start-here", |c| {
        c.strata_start = Some(c.strata_dir.join("START-HERE.bat"));
    })
    .await;
    installed(&s);
    s.agent.set_managed(Managed::default()).unwrap();
    vector(&s);
    std::thread::sleep(Duration::from_millis(20));
    strata_folder(&s).config("iq3_xxs", config_json("a.gguf"));
    std::thread::sleep(Duration::from_millis(20));
    let done = job(
        &s,
        json!({"kind": "set_speed_projection", "model": "coder-IQ1_M", "on": true}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["active"], true, "{view}");
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_page_shows_each_models_context() {
    let s = setup("models-context-view").await;
    installed(&s);
    config_with_args(
        &s,
        "iq3_xxs",
        json!(["--native", "a.gguf", "--max-context", "65536"]),
    );
    config_with_args(
        &s,
        "coder-iq1_m",
        json!([
            "--native",
            "b.gguf",
            "--max-context",
            "524288",
            "--rope-scaling",
            "yarn",
            "--rope-scale",
            "2"
        ]),
    );
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["context"], 65536, "{view}");
    assert_eq!(model(&view, "coder-IQ1_M")["context"], 524288, "{view}");

    config_with_args(&s, "iq3_xxs", json!(["--native", "a.gguf"]));
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["context"], Value::Null, "{view}");
}

// --- #102 switching the context ---

#[tokio::test]
async fn switching_the_context_rewrites_only_max_context() {
    let s = setup("models-context-set").await;
    installed(&s);
    config_with_args(
        &s,
        "coder-iq1_m",
        json!([
            "--native",
            "b.gguf",
            "--max-context",
            "65536",
            "--kv",
            "int8"
        ]),
    );
    let path = strata_folder(&s).installation().config_path("coder-iq1_m");
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let done = job(
        &s,
        json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 131072}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        args_of(&s, "coder-iq1_m"),
        json!([
            "--native",
            "b.gguf",
            "--max-context",
            "131072",
            "--kv",
            "int8"
        ])
    );
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), old);
    assert_eq!(s.fake.stops.load(Ordering::SeqCst), 0, "an inactive model");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 0, "an inactive model");
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "coder-IQ1_M")["context"], 131072, "{view}");
}

#[tokio::test]
async fn switching_down_from_past_256k_removes_the_rope_scaling() {
    let s = setup("models-context-yarn").await;
    installed(&s);
    config_with_args(
        &s,
        "coder-iq1_m",
        json!([
            "--native",
            "b.gguf",
            "--max-context",
            "524288",
            "--rope-scaling",
            "yarn",
            "--rope-scale",
            "2",
            "--kv",
            "int8"
        ]),
    );
    let done = job(
        &s,
        json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 262144}),
    )
    .await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        args_of(&s, "coder-iq1_m"),
        json!([
            "--native",
            "b.gguf",
            "--max-context",
            "262144",
            "--kv",
            "int8"
        ])
    );
}

#[tokio::test]
async fn switching_the_context_is_refused_outside_what_it_takes() {
    let s = setup("models-context-refused").await;
    installed(&s);
    let before = strata_folder(&s).text("coder-iq1_m");
    for (body, status) in [
        (
            json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 393216}),
            400,
        ),
        (
            json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 524288}),
            400,
        ),
        (
            json!({"kind": "set_context", "model": "coder-IQ1_M", "context": 50000}),
            400,
        ),
        (
            json!({"kind": "set_context", "model": "Q2_0", "context": 65536}),
            409,
        ),
        (
            json!({"kind": "set_context", "model": "nothing", "context": 65536}),
            400,
        ),
    ] {
        let (code, answer) = post(&s, "/strata/jobs", body.clone()).await;
        assert_eq!(code, status, "{body}: {answer}");
    }
    let after = strata_folder(&s).text("coder-iq1_m");
    assert_eq!(before, after);
}

#[tokio::test]
async fn switching_the_context_needs_an_installed_strata() {
    let s = setup("models-context-no-strata").await;
    let body = json!({"kind": "set_context", "model": "IQ3_XXS", "context": 65536});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 409, "{answer}");
}

// --- #103 the context of the active model ---

#[tokio::test]
async fn switching_the_active_models_context_restarts_strata_after_the_running_request() {
    let s = setup("models-context-active").await;
    installed(&s);
    config_with_args(
        &s,
        "iq3_xxs",
        json!(["--native", "a.gguf", "--max-context", "65536"]),
    );
    s.fake.strata_busy.store(true, Ordering::SeqCst);
    let body = json!({"kind": "set_context", "model": "IQ3_XXS", "context": 131072});
    let (code, answer) = post(&s, "/strata/jobs", body).await;
    assert_eq!(code, 202, "{answer}");
    job_until(&s, "waiting for the request", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("request"))
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
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
    let view = get(&s, "/strata").await;
    assert_eq!(model(&view, "IQ3_XXS")["context"], 131072, "{view}");
    assert_eq!(model(&view, "IQ3_XXS")["active"], true, "{view}");
}

// --- #104 a context the active model does not start with ---

/// IQ3_XXS active at 512K, its start script a Strata that writes `lines`
/// and keeps running (`hang`) or ends; gives its args.
fn active_at_512k(s: &Setup, lines: &[&str], hang: bool) -> Value {
    installed(s);
    config_with_args(
        s,
        "iq3_xxs",
        json!([
            "--native",
            "a.gguf",
            "--max-context",
            "524288",
            "--rope-scaling",
            "yarn",
            "--rope-scale",
            "2"
        ]),
    );
    s.fake.command(
        "run-iq3_xxs.bat",
        FakeCommand {
            lines: self::lines(lines),
            hang,
            exit: (!hang).then_some(1),
            ..Default::default()
        },
    );
    args_of(s, "iq3_xxs")
}

/// The job's end, for jobs that wait out `context_start_secs` and two
/// restarts, longer than `finished` waits.
async fn finished_slowly(s: &Setup) -> Value {
    for _ in 0..80 {
        let job = get(s, "/strata/job").await;
        if job["state"] != "running" {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("end of the job not reached");
}

#[tokio::test]
async fn a_context_strata_never_answers_with_goes_back() {
    // The limit well behind the fake Strata's first line.
    let s = setup_with("models-context-no-answer", |c| c.context_start_secs = 3).await;
    let before = active_at_512k(&s, &["loading model"], true);
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let body = json!({"kind": "set_context", "model": "IQ3_XXS", "context": 262144});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 202);
    let done = finished_slowly(&s).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert!(
        done["lines"].to_string().contains("loading model"),
        "Strata's output: {done}"
    );
    assert_eq!(args_of(&s, "iq3_xxs"), before, "the old args");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2, "started again");
}

#[tokio::test]
async fn a_strata_that_ends_with_the_new_context_does_not_wait_out_the_limit() {
    let s = setup_with("models-context-exit", |c| c.context_start_secs = 600).await;
    let before = active_at_512k(&s, &["out of VRAM"], false);
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let done = job(
        &s,
        json!({"kind": "set_context", "model": "IQ3_XXS", "context": 262144}),
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(done["state"], "failed", "{done}");
    assert!(done["lines"].to_string().contains("out of VRAM"), "{done}");
    assert_eq!(args_of(&s, "iq3_xxs"), before, "the old args");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2, "started again");
}

#[tokio::test]
async fn a_context_strata_answers_with_after_a_while_stays() {
    let s = setup_with("models-context-slow", |c| c.context_start_secs = 600).await;
    active_at_512k(&s, &[], true);
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let body = json!({"kind": "set_context", "model": "IQ3_XXS", "context": 262144});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 202);
    job_until(&s, "waiting for Strata", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("ready"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.fake.strata_up.store(true, Ordering::SeqCst);
    assert_eq!(finished(&s).await["state"], "done");
    assert_eq!(
        args_of(&s, "iq3_xxs"),
        json!(["--native", "a.gguf", "--max-context", "262144"])
    );
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_the_wait_for_the_new_context_brings_the_old_one_back() {
    let s = setup_with("models-context-cancel", |c| c.context_start_secs = 600).await;
    let before = active_at_512k(&s, &[], true);
    s.fake.strata_stays_down.store(true, Ordering::SeqCst);
    let body = json!({"kind": "set_context", "model": "IQ3_XXS", "context": 262144});
    assert_eq!(post(&s, "/strata/jobs", body).await.0, 202);
    job_until(&s, "waiting for Strata", |j| {
        j["step"].as_str().is_some_and(|t| t.contains("ready"))
    })
    .await;
    let (code, _) = post(&s, "/strata/job/cancel", json!({})).await;
    assert_eq!(code, 202);
    assert_eq!(finished_slowly(&s).await["state"], "cancelled");
    assert_eq!(args_of(&s, "iq3_xxs"), before, "the old args");
    assert_eq!(s.fake.starts.load(Ordering::SeqCst), 2, "started again");
}

// --- The protocol's samples ---

/// The keys in `actual` that `sample` lacks, at every level; a list is
/// held against the sample's first entry.
fn keys_missing(actual: &Value, sample: &Value, at: &str, missing: &mut Vec<String>) {
    match (actual, sample) {
        (Value::Object(actual), Value::Object(sample)) => {
            for (key, value) in actual {
                let path = format!("{at}.{key}");
                match sample.get(key) {
                    Some(expected) => keys_missing(value, expected, &path, missing),
                    None => missing.push(path),
                }
            }
        }
        (Value::Array(actual), Value::Array(sample)) => {
            if let Some(expected) = sample.first() {
                for (i, value) in actual.iter().enumerate() {
                    keys_missing(value, expected, &format!("{at}[{i}]"), missing);
                }
            }
        }
        _ => {}
    }
}

fn sample(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pc-protocol/tests/samples")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Every field the service sends shows in the protocol's sample of that
/// answer, so a new field fails here until its sample has it.
#[tokio::test]
async fn the_protocol_samples_show_every_field_the_service_sends() {
    let s = setup("models-samples").await;
    installed(&s);
    upstream_git(&s);
    bench(
        &s,
        &[
            "  chat decode tok/s   38.50   (37.10 - 39.20, n=3)",
            "  4K prompt tok/s    612.00   (600.00 - 620.00, n=3)",
        ],
    );
    let job = job(&s, json!({"kind": "benchmark"})).await;
    let answers = [
        ("status.json", get(&s, "/status").await),
        ("strata.json", view_once_checked(&s).await),
        ("strata-job.json", job),
        (
            "strata-kill.json",
            post(&s, "/strata/kill", json!({})).await.1,
        ),
        (
            "sleep-force.json",
            post(&s, "/sleep/force", json!({})).await.1,
        ),
    ];
    let mut missing = Vec::new();
    for (name, answer) in &answers {
        keys_missing(answer, &sample(name), name, &mut missing);
    }
    assert!(missing.is_empty(), "the samples lack {missing:?}");
}
