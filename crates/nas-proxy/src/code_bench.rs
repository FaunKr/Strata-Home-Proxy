//! The code benchmark: the active model solves three fixed tasks with
//! opencode in the container `code-bench` (tools/code-bench). The proxy
//! hands the runner a key of its own for the run, which goes through the
//! proxy like any client's requests, and keeps each model's last result.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use strata_admin_ui::api::{CodeBenchAll, CodeBenchPage, CodeBenchRun, CodeBenchState};
use strata_pc_protocol::{StrataJobRequest, StrataJobState};

use crate::{
    api_keys::KeyError, clock, events::EventKind, files::write_private, state::AppState, timeouts,
};

const RESULTS_FILE: &str = "code-bench.json";
/// The prefix of the run's key, so a restarted proxy still knows which key
/// is its own.
const KEY_FILE: &str = "code-bench-key";
/// The model active before a run over all models, so a restarted proxy brings
/// it back after the run it was following.
const RESTORE_FILE: &str = "code-bench-restore";
/// The name of the run's key under Access; it is revoked when the run ends.
pub const KEY_NAME: &str = "Code benchmark";
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

/// Where a run over all models stands: the models in order, the one being
/// benchmarked now and the task being solved, for the card's progress.
struct All {
    api: CodeBenchAll,
    /// The models in the order the run goes through them.
    ids: Vec<String>,
}

pub struct CodeBench {
    /// The running run, or the last one that did not finish.
    run: Mutex<Option<CodeBenchRun>>,
    /// Each model's last finished run; a model that failed gets its error row.
    results: Mutex<Vec<CodeBenchRun>>,
    /// A start is on its way to the runner.
    starting: AtomicBool,
    /// The prefix of the key the proxy made for the run.
    key: Mutex<Option<String>>,
    dir: Option<PathBuf>,
    /// A run over all models is going, or was the last one.
    all: Mutex<Option<All>>,
    /// A run over all models was cancelled: stop after the model that runs now.
    cancelled: AtomicBool,
    /// The model active before a run over all models, kept so a restarted proxy
    /// brings it back.
    restore: Mutex<Option<String>>,
    /// How long the proxy waits between asks of the runner.
    poll: Mutex<Duration>,
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
        let restore = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(RESTORE_FILE)).ok())
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        Self {
            run: Mutex::new(None),
            results: Mutex::new(results),
            starting: AtomicBool::new(false),
            key: Mutex::new(key),
            dir: data_dir,
            all: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            restore: Mutex::new(restore),
            poll: Mutex::new(POLL),
        }
    }

    /// Shortens the wait between asks of the runner, so a test need not sit
    /// through `POLL`.
    pub fn set_poll(&self, poll: Duration) {
        *self.poll.lock().unwrap() = poll;
    }

    /// Whether a run is on its way or going, single or over all models: Strata
    /// jobs wait meanwhile.
    pub fn busy(&self) -> bool {
        self.running() || self.starting.load(Ordering::SeqCst) || self.all.lock().unwrap().is_some()
    }

    fn running(&self) -> bool {
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
                tracing::error!("{KEY_FILE} not saved: {e}");
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
                tracing::error!("{RESULTS_FILE} not saved: {e}");
            }
        }
    }

    /// A model that failed to load or whose run failed: its error row.
    fn add_row(&self, model: &str, error: &str) {
        self.keep(CodeBenchRun {
            model: model.to_string(),
            state: CodeBenchState::Failed,
            error: Some(error.to_string()),
            started: clock::unix_now(),
            ended: Some(clock::unix_now()),
            tasks: vec![],
            at_text: String::new(),
        });
    }

    /// The model active before a run over all models, kept for a restart.
    fn set_restore(&self, model: &str) {
        if let Some(dir) = &self.dir
            && let Err(e) = write_private(dir, RESTORE_FILE, model.as_bytes())
        {
            tracing::error!("{RESTORE_FILE} not saved: {e}");
        }
        *self.restore.lock().unwrap() = Some(model.to_string());
    }

    fn restore_model(&self) -> Option<String> {
        self.restore.lock().unwrap().clone()
    }

    /// The run over all models for the card, or `None` when none was started.
    fn all_progress(&self) -> Option<CodeBenchAll> {
        self.all.lock().unwrap().as_ref().map(|all| all.api.clone())
    }

    fn mark_task(&self, task: usize) {
        if let Some(all) = self.all.lock().unwrap().as_mut() {
            all.api.task = task;
        }
    }

    /// Whether a model's result is kept, for a test that waits for one.
    pub fn has_result(&self, model: &str) -> bool {
        self.results
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.model == model && !r.tasks.is_empty())
    }

    /// Whether a model's error row is there, for a test that waits for one.
    pub fn error_row_for(&self, model: &str) -> bool {
        self.results
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.model == model && r.error.is_some())
    }
}

pub async fn page(state: &AppState) -> CodeBenchPage {
    let problem = match state.cfg().code_bench_url.clone() {
        None => Some(NO_URL.to_string()),
        // While a run goes, `follow` asks the runner already.
        Some(_) if state.code_bench.running() => None,
        Some(url) => health(state, &url).await.err(),
    };
    let with_time = |mut run: CodeBenchRun| {
        run.at_text = clock::local_text(run.ended.unwrap_or(run.started));
        run
    };
    // Sorted by hidden tests passed, then by the time the run took.
    let mut results = state
        .code_bench
        .results
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(with_time)
        .collect::<Vec<_>>();
    results.sort_by(|a, b| {
        let passed = |r: &CodeBenchRun| r.tasks.iter().map(|t| t.passed).sum::<u32>();
        let secs = |r: &CodeBenchRun| r.tasks.iter().filter_map(|t| t.total_secs).sum::<f64>();
        passed(b).cmp(&passed(a)).then(secs(a).total_cmp(&secs(b)))
    });
    let models = state
        .strata
        .last_info()
        .map(|info| info.models.len())
        .unwrap_or(0);
    CodeBenchPage {
        problem,
        run: state.code_bench.run.lock().unwrap().clone().map(with_time),
        results,
        all: state.code_bench.all_progress(),
        models,
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
const GAMING: &str = "Gaming mode is on.";
const STRATA_JOB: &str = "A Strata job is running. Start the code benchmark once it ends.";
const FEWER: &str =
    "A run over all models needs at least two models. Fewer than two installed models are there.";

/// Clears the start guard however `start` ends.
struct Starting<'a>(&'a AtomicBool);

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub async fn start(state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let url = state.cfg().code_bench_url.clone().ok_or(NO_URL)?;
    if state.code_bench.all.lock().unwrap().is_some()
        || state.code_bench.running()
        || state.code_bench.starting.swap(true, Ordering::SeqCst)
    {
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
        return Err(STRATA_JOB.into());
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
        // After `active_model`, so from the speeds the PC service just reported.
        "timeout_secs": timeouts::code_bench_task(state).as_secs(),
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

/// Starts a run over all installed models: each is activated and solves the
/// tasks once; the model active before the run comes back at the end. One key
/// serves the whole run and is revoked then.
pub async fn start_all(state: &Arc<AppState>) -> Result<(), String> {
    let url = state.cfg().code_bench_url.clone().ok_or(NO_URL)?;
    if state.code_bench.all.lock().unwrap().is_some()
        || state.code_bench.running()
        || state.code_bench.starting.swap(true, Ordering::SeqCst)
    {
        return Err(RUNNING.into());
    }
    let _starting = Starting(&state.code_bench.starting);
    health(state, &url).await?;
    // A run the proxy lost, e.g. by a restart, goes on: follow it again.
    if let Some(run) = current(state, &url).await
        && run.state == CodeBenchState::Running
    {
        *state.code_bench.run.lock().unwrap() = Some(run);
        let state = Arc::clone(state);
        let url = url.clone();
        tokio::spawn(async move { follow(&state, &url).await });
        return Err(RUNNING.into());
    }
    let page = crate::strata::page(state).await;
    if page.gaming {
        return Err(GAMING.into());
    }
    if state.strata.job_running() {
        return Err(STRATA_JOB.into());
    }
    let info = page.info.ok_or_else(|| {
        String::from(
            "Which models are installed is not known. Open the Strata page while the PC is awake.",
        )
    })?;
    let ids: Vec<String> = info.models.iter().map(|m| m.id.clone()).collect();
    if ids.len() < 2 {
        return Err(FEWER.into());
    }
    let restore = info
        .models
        .iter()
        .find(|m| m.active)
        .map(|m| m.id.clone())
        .ok_or_else(|| {
            String::from(
                "Which model is active is not known. Open the Strata page while the PC is awake.",
            )
        })?;
    revoke_key(state);
    let (key, info) = state.api_keys.create(KEY_NAME).map_err(|e| match e {
        KeyError::DuplicateName => format!(
            "A key named \"{KEY_NAME}\" exists under Access. Rename it: the code benchmark makes a key of that name for each run."
        ),
        e => e.to_string(),
    })?;
    state.code_bench.set_key(Some(info.prefix));
    state.code_bench.set_restore(&restore);
    *state.code_bench.all.lock().unwrap() = Some(All {
        api: CodeBenchAll {
            models: ids.len(),
            model: 0,
            tasks: TASKS.len(),
            task: 0,
            restore: restore.clone(),
        },
        ids,
    });
    let state = Arc::clone(state);
    let url = url.clone();
    tokio::spawn(async move { run_all(&state, &url, &key).await });
    Ok(())
}

/// Goes through the models in order, then brings back the one active before the
/// run. A model that fails to load or whose run fails gets its error row and
/// the run goes on; a Cancel stops it after the model that runs now.
async fn run_all(state: &Arc<AppState>, url: &str, key: &str) {
    let ids = {
        let all = state.code_bench.all.lock().unwrap();
        all.as_ref()
            .map(|all| (all.ids.clone(), all.api.restore.clone()))
    };
    let Some((ids, restore)) = ids else {
        return;
    };
    for (n, id) in ids.iter().enumerate() {
        if state.code_bench.cancelled.load(Ordering::SeqCst) {
            break;
        }
        {
            let mut all = state.code_bench.all.lock().unwrap();
            let all = all.as_mut().expect("the run is over all models");
            all.api.model = n + 1;
            all.api.task = 0;
        }
        if let Err(e) = activate(state, id).await {
            state.code_bench.add_row(id, &e);
            continue;
        }
        if let Err(e) = run_model(state, url, id, key).await {
            state.code_bench.add_row(id, &e);
            continue;
        }
    }
    if let Err(e) = activate(state, &restore).await {
        state.code_bench.add_row(&restore, &e);
    }
    *state.code_bench.all.lock().unwrap() = None;
    revoke_key(state);
    state.overview.refresh_now();
}

/// Activates one model through the PC service and waits for the job. The
/// proxy's own run guard stays up meanwhile, so a Strata job started by hand
/// is refused until the run ends.
async fn activate(state: &Arc<AppState>, model: &str) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err("The PC is not set up.".into());
    }
    let request = StrataJobRequest::Activate {
        model: model.to_string(),
    };
    state.strata.set_job_running(true);
    agent.start_strata_job(&request).await?;
    state.events.record(
        EventKind::StrataJobStarted,
        Some(format!("Activate {model}")),
    );
    let mut missed = 0;
    loop {
        let poll = *state.code_bench.poll.lock().unwrap();
        tokio::time::sleep(poll).await;
        let Some(job) = agent.strata_job().await else {
            missed += 1;
            if missed < LOST_AFTER {
                continue;
            }
            return Err("The PC service stopped answering.".into());
        };
        missed = 0;
        match job.state {
            StrataJobState::Running => {}
            StrataJobState::Done => {
                state.strata.set_job_running(false);
                return Ok(());
            }
            _ => {
                state.strata.set_job_running(false);
                return Err(job
                    .error
                    .as_deref()
                    .unwrap_or("the model did not load")
                    .to_string());
            }
        }
    }
}

/// Hands the runner the tasks for one model, with the key made for the run.
async fn run_model(state: &Arc<AppState>, url: &str, model: &str, key: &str) -> Result<(), String> {
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
        "timeout_secs": timeouts::code_bench_task(state).as_secs(),
    });
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
    let run = serde_json::from_value::<CodeBenchRun>(value)
        .map_err(|e| format!("The code-bench container answered something unknown: {e}"))?;
    *state.code_bench.run.lock().unwrap() = Some(run);
    follow_run(state, url, true).await;
    Ok(())
}

/// After a restart: follows a run that went on meanwhile, or revokes the key
/// a run left behind. A run over all models is not resumed; when the run the
/// proxy was following ends, the model active before it comes back.
pub fn resume(state: &Arc<AppState>) {
    let Some(url) = state.cfg().code_bench_url.clone() else {
        return;
    };
    if state.code_bench.key.lock().unwrap().is_none() {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        match current(&state, &url).await {
            Some(run) if run.state == CodeBenchState::Running => {
                *state.code_bench.run.lock().unwrap() = Some(run);
                let was_over_all = state.code_bench.restore_model().is_some();
                let ended = follow_run(&state, &url, was_over_all).await;
                if was_over_all && ended.state != CodeBenchState::Cancelled {
                    state.code_bench.keep(ended);
                    if let Some(restore) = state.code_bench.restore_model()
                        && let Err(e) = activate(&state, &restore).await
                    {
                        state.code_bench.add_row(&restore, &e);
                    }
                }
            }
            _ => {}
        }
        revoke_key(&state);
        state.overview.refresh_now();
    });
}

pub async fn cancel(state: &AppState) -> Result<(), String> {
    let url = state.cfg().code_bench_url.clone().ok_or(NO_URL)?;
    // A run over all models: stop after the model that runs now, then bring
    // back the model active before the run and revoke the key.
    if state.code_bench.all.lock().unwrap().is_some() {
        state.code_bench.cancelled.store(true, Ordering::SeqCst);
    }
    // Asked of the runner even when the proxy knows of no run: it may have
    // lost one, e.g. by a restart.
    let resp = state
        .probe_client()
        .post(format!("{url}/runs/current/cancel"))
        .send()
        .await
        .map_err(|e| format!("The code-bench container does not answer: {e}"))?;
    // 409: no run, or it ended meanwhile and the next poll notes how. A run
    // over all models is cancelled by its flag, so 409 is fine while one goes.
    if resp.status().as_u16() == 409
        && !state.code_bench.running()
        && state.code_bench.all.lock().unwrap().is_none()
    {
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
async fn active_model(state: &std::sync::Arc<AppState>) -> Result<String, String> {
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
async fn follow(state: &Arc<AppState>, url: &str) {
    follow_run(state, url, false).await;
    revoke_key(state);
    state.overview.refresh_now();
}

/// One run to its end. As part of a run over all models (`in_all`) it keeps a
/// failed run as its error row and leaves the shared key for the whole run;
/// it returns the run as it ended.
async fn follow_run(state: &Arc<AppState>, url: &str, in_all: bool) -> CodeBenchRun {
    let mut missed = 0;
    let last;
    loop {
        let poll = *state.code_bench.poll.lock().unwrap();
        tokio::time::sleep(poll).await;
        let Some(run) = current(state, url).await else {
            missed += 1;
            if missed < LOST_AFTER {
                continue;
            }
            last = {
                let mut guard = state.code_bench.run.lock().unwrap();
                let run = guard.get_or_insert_with(|| CodeBenchRun {
                    model: String::new(),
                    state: CodeBenchState::Failed,
                    error: None,
                    started: clock::unix_now(),
                    ended: None,
                    tasks: vec![],
                    at_text: String::new(),
                });
                run.state = CodeBenchState::Failed;
                run.error = Some("The code-bench container stopped answering.".into());
                run.ended = Some(clock::unix_now());
                run.clone()
            };
            break;
        };
        missed = 0;
        match run.state {
            CodeBenchState::Running => {
                if in_all {
                    state.code_bench.mark_task(
                        run.tasks
                            .iter()
                            .position(|t| t.state == "running")
                            .unwrap_or(0)
                            + 1,
                    );
                }
                *state.code_bench.run.lock().unwrap() = Some(run);
            }
            CodeBenchState::Done => {
                state.code_bench.keep(run.clone());
                *state.code_bench.run.lock().unwrap() = None;
                last = run;
                break;
            }
            CodeBenchState::Cancelled | CodeBenchState::Failed => {
                if in_all && run.state == CodeBenchState::Failed {
                    state.code_bench.keep(run.clone());
                }
                *state.code_bench.run.lock().unwrap() = Some(run.clone());
                last = run;
                break;
            }
        }
    }
    if !in_all {
        state.overview.refresh_now();
    }
    last
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
        tracing::error!("Code benchmark key not revoked: {e}");
        return;
    }
    state.code_bench.set_key(None);
}
