//! The code benchmark: the active model solves three fixed tasks with
//! opencode in the container `code-bench` (tools/code-bench). The proxy
//! hands the runner a key of its own for the run, which goes through the
//! proxy like any client's requests, and keeps each model's last result.

use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use strata_admin_ui::api::{CodeBenchPage, CodeBenchRun, CodeBenchState};

use crate::{api_keys::KeyError, clock, files::write_private, state::AppState};

const RESULTS_FILE: &str = "code-bench.json";
/// The prefix of the run's key, so a restarted proxy still knows which key
/// is its own.
const KEY_FILE: &str = "code-bench-key";
/// The name of the run's key under Access; it is revoked when the run ends.
pub const KEY_NAME: &str = "Code benchmark";
/// How long opencode may work on one task.
const TASK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_secs(2);
/// Unanswered polls after which the run counts as lost, e.g. after the
/// container restarted.
const LOST_AFTER: u32 = 30;
const NO_URL: &str =
    "No code benchmark is set up. Set its address (CODE_BENCH_URL) under Strata → Connection.";

/// A task as the runner takes it: the files opencode sees, and the hidden
/// tests that score it afterwards.
struct Task {
    name: &'static str,
    title: &'static str,
    task: &'static str,
    example: &'static str,
    hidden: &'static str,
}

const TASKS: [Task; 3] = [
    Task {
        name: "roman",
        title: "Roman numerals",
        task: include_str!("../../../tools/code-bench/tasks/roman/TASK.md"),
        example: include_str!("../../../tools/code-bench/tasks/roman/example_test.py"),
        hidden: include_str!("../../../tools/code-bench/tasks/roman/hidden_test.py"),
    },
    Task {
        name: "csv-summary",
        title: "CSV summary",
        task: include_str!("../../../tools/code-bench/tasks/csv-summary/TASK.md"),
        example: include_str!("../../../tools/code-bench/tasks/csv-summary/example_test.py"),
        hidden: include_str!("../../../tools/code-bench/tasks/csv-summary/hidden_test.py"),
    },
    Task {
        name: "lru-cache",
        title: "LRU cache",
        task: include_str!("../../../tools/code-bench/tasks/lru-cache/TASK.md"),
        example: include_str!("../../../tools/code-bench/tasks/lru-cache/example_test.py"),
        hidden: include_str!("../../../tools/code-bench/tasks/lru-cache/hidden_test.py"),
    },
];

pub struct CodeBench {
    /// The running run, or the last one that did not finish.
    run: Mutex<Option<CodeBenchRun>>,
    /// Each model's last finished run.
    results: Mutex<Vec<CodeBenchRun>>,
    /// A start is on its way to the runner.
    starting: AtomicBool,
    /// The prefix of the key the proxy made for the run.
    key: Mutex<Option<String>>,
    dir: Option<PathBuf>,
}

impl CodeBench {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let results = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(RESULTS_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let key = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(KEY_FILE)).ok())
            .map(|prefix| prefix.trim().to_string())
            .filter(|prefix| !prefix.is_empty());
        Self {
            run: Mutex::new(None),
            results: Mutex::new(results),
            starting: AtomicBool::new(false),
            key: Mutex::new(key),
            dir: data_dir,
        }
    }

    pub fn running(&self) -> bool {
        self.run
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| r.state == CodeBenchState::Running)
    }

    fn set_key(&self, prefix: Option<String>) {
        if let Some(dir) = &self.dir {
            let text = prefix.clone().unwrap_or_default();
            if let Err(e) = write_private(dir, KEY_FILE, text.as_bytes()) {
                tracing::error!("{KEY_FILE} nicht gespeichert: {e}");
            }
        }
        *self.key.lock().unwrap() = prefix;
    }

    /// Keeps a finished run in place of the model's last one.
    fn keep(&self, run: CodeBenchRun) {
        let mut results = self.results.lock().unwrap();
        results.retain(|r| r.model != run.model);
        results.push(run);
        if let Some(dir) = &self.dir {
            let bytes = serde_json::to_vec_pretty(&*results).expect("results serialize");
            if let Err(e) = write_private(dir, RESULTS_FILE, &bytes) {
                tracing::error!("{RESULTS_FILE} nicht gespeichert: {e}");
            }
        }
    }
}

pub async fn page(state: &AppState) -> CodeBenchPage {
    let problem = match state.cfg().code_bench_url.clone() {
        None => Some(NO_URL.to_string()),
        Some(url) => health(state, &url).await.err(),
    };
    let with_time = |mut run: CodeBenchRun| {
        run.at_text = clock::local_text(run.ended.unwrap_or(run.started));
        run
    };
    CodeBenchPage {
        problem,
        run: state.code_bench.run.lock().unwrap().clone().map(with_time),
        results: state
            .code_bench
            .results
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(with_time)
            .collect(),
    }
}

/// Whether the runner answers.
async fn health(state: &AppState, url: &str) -> Result<(), String> {
    let down = || format!("The code-bench container does not answer at {url}.");
    let resp = state
        .probe_client()
        .get(format!("{url}/health"))
        .send()
        .await
        .map_err(|_| down())?;
    if !resp.status().is_success() {
        return Err(down());
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| down())?;
    if body["opencode"].is_null() {
        return Err("opencode is missing in the code-bench container.".into());
    }
    // E.g. its firewall for the agent could not be set.
    if let Some(problem) = body["problem"].as_str() {
        return Err(problem.to_string());
    }
    Ok(())
}

const RUNNING: &str = "A code benchmark is running already.";

/// Clears the start guard however `start` ends.
struct Starting<'a>(&'a AtomicBool);

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub async fn start(state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let url = state.cfg().code_bench_url.clone().ok_or(NO_URL)?;
    if state.code_bench.running() || state.code_bench.starting.swap(true, Ordering::SeqCst) {
        return Err(RUNNING.into());
    }
    let _starting = Starting(&state.code_bench.starting);
    health(state, &url).await?;
    // A run the proxy lost, e.g. by a restart, goes on: follow it again.
    if let Some(run) = current(state, &url).await
        && run.state == CodeBenchState::Running
    {
        *state.code_bench.run.lock().unwrap() = Some(run);
        let state = std::sync::Arc::clone(state);
        tokio::spawn(async move { follow(&state, &url).await });
        return Err(RUNNING.into());
    }
    if state.strata.job_running() {
        return Err("A Strata job is running. Start the code benchmark once it ends.".into());
    }
    let model = active_model(state).await?;
    revoke_key(state);
    let (key, info) = state.api_keys.create(KEY_NAME).map_err(|e| match e {
        KeyError::DuplicateName => format!(
            "A key named \"{KEY_NAME}\" exists under Access. Rename it: the code benchmark makes a key of that name for each run."
        ),
        e => e.to_string(),
    })?;
    state.code_bench.set_key(Some(info.prefix));
    let tasks: Vec<_> = TASKS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "files": {"TASK.md": t.task, "example_test.py": t.example},
                "hidden_test": t.hidden,
            })
        })
        .collect();
    let body = json!({
        "model": model,
        "api_key": key,
        "tasks": tasks,
        "timeout_secs": TASK_TIMEOUT.as_secs(),
    });
    let answer = async {
        let resp = state
            .probe_client()
            .post(format!("{url}/runs"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("The code-bench container does not answer: {e}"))?;
        let code = resp.status();
        let value: serde_json::Value = resp.json().await.unwrap_or_default();
        if !code.is_success() {
            return Err(value["reason"].as_str().map_or_else(
                || format!("The code-bench container answered {code}."),
                String::from,
            ));
        }
        serde_json::from_value::<CodeBenchRun>(value)
            .map_err(|e| format!("The code-bench container answered something unknown: {e}"))
    }
    .await;
    let run = match answer {
        Ok(run) => run,
        Err(e) => {
            revoke_key(state);
            return Err(e);
        }
    };
    *state.code_bench.run.lock().unwrap() = Some(run);
    drop(_starting);
    let state = std::sync::Arc::clone(state);
    tokio::spawn(async move { follow(&state, &url).await });
    Ok(())
}

pub async fn cancel(state: &AppState) -> Result<(), String> {
    let url = state.cfg().code_bench_url.clone().ok_or(NO_URL)?;
    // Asked of the runner even when the proxy knows of no run: it may have
    // lost one, e.g. by a restart.
    let resp = state
        .probe_client()
        .post(format!("{url}/runs/current/cancel"))
        .send()
        .await
        .map_err(|e| format!("The code-bench container does not answer: {e}"))?;
    // 409: no run, or it ended meanwhile and the next poll notes how.
    if resp.status().as_u16() == 409 && !state.code_bench.running() {
        return Err("No code benchmark is running.".into());
    }
    if !resp.status().is_success() && resp.status().as_u16() != 409 {
        return Err(format!(
            "The code-bench container answered {}.",
            resp.status()
        ));
    }
    Ok(())
}

/// The model the PC service reports as active, now or when it last answered.
async fn active_model(state: &AppState) -> Result<String, String> {
    let page = crate::strata::page(state).await;
    page.info
        .and_then(|info| info.models.into_iter().find(|m| m.active))
        .map(|m| m.id)
        .ok_or_else(|| {
            "Which model is active is not known. Open the Strata page while the PC is awake.".into()
        })
}

/// Asks the runner for the run until it ends, then revokes its key and keeps
/// the result.
async fn follow(state: &AppState, url: &str) {
    let mut missed = 0;
    loop {
        tokio::time::sleep(POLL).await;
        let Some(run) = current(state, url).await else {
            missed += 1;
            if missed < LOST_AFTER {
                continue;
            }
            let mut current = state.code_bench.run.lock().unwrap();
            if let Some(run) = current.as_mut() {
                run.state = CodeBenchState::Failed;
                run.error = Some("The code-bench container stopped answering.".into());
                run.ended = Some(clock::unix_now());
            }
            break;
        };
        missed = 0;
        match run.state {
            CodeBenchState::Running => *state.code_bench.run.lock().unwrap() = Some(run),
            CodeBenchState::Done => {
                state.code_bench.keep(run);
                *state.code_bench.run.lock().unwrap() = None;
                break;
            }
            CodeBenchState::Cancelled | CodeBenchState::Failed => {
                *state.code_bench.run.lock().unwrap() = Some(run);
                break;
            }
        }
    }
    revoke_key(state);
    state.overview.refresh_now();
}

/// The runner's current run; `None` when it does not answer or had none yet.
async fn current(state: &AppState, url: &str) -> Option<CodeBenchRun> {
    let resp = state
        .probe_client()
        .get(format!("{url}/runs/current"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<CodeBenchRun>().await.ok()
}

/// Revokes the key the proxy made for the run, also one left over from a run
/// the proxy lost; never a key someone else named alike.
fn revoke_key(state: &AppState) {
    let Some(prefix) = state.code_bench.key.lock().unwrap().clone() else {
        return;
    };
    let ours = state
        .api_keys
        .list()
        .into_iter()
        .find(|k| k.name == KEY_NAME && k.prefix == prefix);
    if let Some(key) = ours
        && let Err(e) = state.api_keys.revoke(&key.name, &key.prefix)
    {
        tracing::error!("Key des Code-Benchmarks nicht widerrufen: {e}");
        return;
    }
    state.code_bench.set_key(None);
}
