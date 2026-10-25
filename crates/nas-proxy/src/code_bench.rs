//! The code benchmark: the active model solves four fixed tasks with
//! opencode in the container `code-bench` (tools/code-bench): three Python
//! tasks, and a real feature of this repo on a checkout in the runner's
//! image. The proxy
//! hands the runner a key of its own for the run, which goes through the
//! proxy like any client's requests, and keeps each model's last result.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use strata_admin_ui::api::{
    CodeBenchAll, CodeBenchModelRuns, CodeBenchPage, CodeBenchRun, CodeBenchState,
    CodeBenchTaskInfo, RunSettings,
};
use strata_pc_protocol::{StrataInfo, StrataJobRequest, StrataJobState};
use tokio::sync::watch;

use crate::{
    api_keys::{KeyError, ModelName},
    clock,
    events::EventKind,
    files::write_private,
    speed_history::run_settings,
    state::{ActivityGuard, AppState},
    timeouts,
};

const RESULTS_FILE: &str = "code-bench.json";
/// Runs kept per model; the oldest go first.
const KEPT: usize = 20;
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

/// A task as the runner takes it: the brief opencode reads, and how it is
/// scored.
struct Task {
    name: &'static str,
    title: &'static str,
    /// TASK.md, the brief.
    task: &'static str,
    kind: Kind,
    /// Its time limit, in multiples of the per-task limit.
    limit_factor: u32,
}

enum Kind {
    /// Python: the example tests opencode sees, the hidden ones that score.
    Files {
        example: &'static str,
        hidden: &'static str,
    },
    /// A checkout in the runner's image (tools/code-bench/prepare_repo_task.py),
    /// scored there by the feature's own tests.
    Repo,
}

const TASKS: [Task; 4] = [
    Task {
        name: "roman",
        title: "Roman numerals",
        task: include_str!("../../../tools/code-bench/tasks/roman/TASK.md"),
        kind: Kind::Files {
            example: include_str!("../../../tools/code-bench/tasks/roman/example_test.py"),
            hidden: include_str!("../../../tools/code-bench/tasks/roman/hidden_test.py"),
        },
        limit_factor: 1,
    },
    Task {
        name: "csv-summary",
        title: "CSV summary",
        task: include_str!("../../../tools/code-bench/tasks/csv-summary/TASK.md"),
        kind: Kind::Files {
            example: include_str!("../../../tools/code-bench/tasks/csv-summary/example_test.py"),
            hidden: include_str!("../../../tools/code-bench/tasks/csv-summary/hidden_test.py"),
        },
        limit_factor: 1,
    },
    Task {
        name: "lru-cache",
        title: "LRU cache",
        task: include_str!("../../../tools/code-bench/tasks/lru-cache/TASK.md"),
        kind: Kind::Files {
            example: include_str!("../../../tools/code-bench/tasks/lru-cache/example_test.py"),
            hidden: include_str!("../../../tools/code-bench/tasks/lru-cache/hidden_test.py"),
        },
        limit_factor: 1,
    },
    // A change in Rust in a large codebase takes a local model far longer.
    Task {
        name: "pc-force-endpoints",
        title: "Forced sleep and restart (Rust, this repo)",
        task: include_str!("../../../tools/code-bench/tasks/pc-force-endpoints/TASK.md"),
        kind: Kind::Repo,
        limit_factor: 2,
    },
];

/// The tasks with their limits now.
fn task_infos(state: &AppState) -> Vec<CodeBenchTaskInfo> {
    let base = timeouts::code_bench_task(state).as_secs();
    TASKS
        .iter()
        .map(|t| CodeBenchTaskInfo {
            name: t.name.into(),
            title: t.title.into(),
            limit_secs: base * u64::from(t.limit_factor),
        })
        .collect()
}

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
    /// The kept runs, finished and failed, at most `KEPT` per model.
    kept: Mutex<Vec<CodeBenchRun>>,
    /// The id the next kept run gets.
    next_id: AtomicU64,
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
    /// A run over all models holds the requests of other keys: true while
    /// it goes. Not kept over a restart, so a restarted proxy holds no one.
    holding: watch::Sender<bool>,
}

impl CodeBench {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let results: Vec<CodeBenchRun> = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(RESULTS_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let (results, migrated) = from_older(results);
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
        let next_id = results.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let bench = Self {
            run: Mutex::new(None),
            kept: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(next_id),
            starting: AtomicBool::new(false),
            key: Mutex::new(key),
            dir: data_dir,
            all: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            restore: Mutex::new(restore),
            poll: Mutex::new(POLL),
            holding: watch::Sender::new(false),
        };
        if migrated {
            bench.save(&results);
        }
        *bench.kept.lock().unwrap() = results;
        bench
    }

    /// Whether a request with `raw_key` waits: a run over all models goes and
    /// the key is not the run's own.
    pub fn holds(&self, raw_key: &str) -> bool {
        *self.holding.borrow()
            && !self
                .key
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|prefix| raw_key.starts_with(prefix))
    }

    /// Waits until the run over all models ends, at most `limit`; whether it
    /// ended.
    pub async fn wait_for_end(&self, limit: Duration) -> bool {
        let mut ended = self.holding.subscribe();
        tokio::time::timeout(limit, ended.wait_for(|holding| !*holding))
            .await
            .is_ok_and(|r| r.is_ok())
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

    /// Keeps a run with what the model ran with; a model's oldest runs go
    /// beyond `KEPT`.
    fn keep(&self, mut run: CodeBenchRun, settings: Option<RunSettings>) {
        run.id = self.next_id.fetch_add(1, Ordering::SeqCst);
        run.settings = settings;
        let mut results = self.kept.lock().unwrap();
        let model = run.model.clone();
        results.push(run);
        let mut of_model: Vec<(u64, u64)> = results
            .iter()
            .filter(|r| r.model == model)
            .map(|r| (ended_or_started(r), r.id))
            .collect();
        if of_model.len() > KEPT {
            of_model.sort();
            let dropped: Vec<u64> = of_model[..of_model.len() - KEPT]
                .iter()
                .map(|(_, id)| *id)
                .collect();
            results.retain(|r| !dropped.contains(&r.id));
        }
        self.save(&results);
    }

    fn save(&self, results: &[CodeBenchRun]) {
        if let Some(dir) = &self.dir {
            let bytes = serde_json::to_vec_pretty(results).expect("results serialize");
            if let Err(e) = write_private(dir, RESULTS_FILE, &bytes) {
                tracing::error!("{RESULTS_FILE} not saved: {e}");
            }
        }
    }

    /// A model that failed to load, or whose run failed: a run of its own,
    /// next to the results it has.
    fn note_failure(&self, model: &str, error: &str, settings: Option<RunSettings>) {
        self.keep(failed_run(model, error, clock::unix_now()), settings);
    }

    /// Each model's latest finished run, the newest failure after it noted
    /// on it; a model that only failed gives its newest failure.
    pub fn results(&self) -> Vec<CodeBenchRun> {
        self.history(None)
            .into_iter()
            .filter_map(|m| {
                let done = m.runs.iter().position(|r| r.state == CodeBenchState::Done);
                match done {
                    Some(i) => {
                        let mut run = m.runs[i].clone();
                        if let Some(failed) = m.runs[..i].first() {
                            run.failed_later = failed.error.clone();
                            run.failed_at = Some(ended_or_started(failed));
                        }
                        Some(run)
                    }
                    None => m.runs.first().cloned(),
                }
            })
            .collect()
    }

    /// Each model's runs, newest first; `info` says which models are
    /// installed.
    pub fn history(&self, info: Option<&StrataInfo>) -> Vec<CodeBenchModelRuns> {
        let results = self.kept.lock().unwrap();
        let mut models: Vec<CodeBenchModelRuns> = Vec::new();
        for run in results.iter() {
            match models.iter_mut().find(|m| m.model == run.model) {
                Some(m) => m.runs.push(run.clone()),
                None => models.push(CodeBenchModelRuns {
                    model: run.model.clone(),
                    installed: info.is_some_and(|i| i.models.iter().any(|m| m.id == run.model)),
                    runs: vec![run.clone()],
                }),
            }
        }
        for m in &mut models {
            m.runs
                .sort_by_key(|r| std::cmp::Reverse((ended_or_started(r), r.id)));
        }
        models
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

    /// The run over all models ended: no model to bring back after a restart.
    fn clear_restore(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(dir.join(RESTORE_FILE));
        }
        *self.restore.lock().unwrap() = None;
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
}

/// When a run ended, or started if it has no end.
fn ended_or_started(run: &CodeBenchRun) -> u64 {
    run.ended.unwrap_or(run.started)
}

/// A run of `model` that failed with `error` at `at`, without tasks.
fn failed_run(model: &str, error: &str, at: u64) -> CodeBenchRun {
    CodeBenchRun {
        model: model.to_string(),
        state: CodeBenchState::Failed,
        error: Some(error.to_string()),
        started: at,
        ended: Some(at),
        tasks: vec![],
        at_text: String::new(),
        id: 0,
        settings: None,
        failed_later: None,
        failed_at: None,
    }
}

/// Runs as an older proxy kept them, one per model with a later failure
/// noted on it, as runs of their own with ids; whether any changed.
fn from_older(runs: Vec<CodeBenchRun>) -> (Vec<CodeBenchRun>, bool) {
    let mut changed = false;
    let mut next = runs.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    let mut out = Vec::new();
    for mut run in runs {
        if run.id == 0 {
            run.id = next;
            next += 1;
            changed = true;
        }
        if let Some(error) = run.failed_later.take() {
            let at = run
                .failed_at
                .take()
                .unwrap_or_else(|| ended_or_started(&run));
            out.push(CodeBenchRun {
                id: next,
                ..failed_run(&run.model, &error, at)
            });
            next += 1;
            changed = true;
        }
        run.failed_at = None;
        out.push(run);
    }
    (out, changed)
}

/// What `model` runs with: from the PC service now, which a run keeps
/// awake, else from its last report.
async fn settings_now(state: &AppState, model: &str) -> Option<RunSettings> {
    let info = match state.agent().strata().await {
        Some((info, _)) => Some(info),
        None => state.strata.last_info(),
    };
    info.map(|info| run_settings(&info, model, None))
}

pub async fn page(state: &AppState) -> CodeBenchPage {
    let problem = match state.cfg().code_bench_url.clone() {
        None => Some(NO_URL.to_string()),
        // While a run goes, `follow` asks the runner already.
        Some(_) if state.code_bench.running() => None,
        Some(url) => health(state, &url).await.err(),
    };
    let with_time = |mut run: CodeBenchRun| {
        run.at_text = clock::local_text(ended_or_started(&run));
        run
    };
    let info = state.strata.last_info();
    let mut history = state.code_bench.history(info.as_ref());
    for m in &mut history {
        m.runs = m.runs.drain(..).map(with_time).collect();
    }
    // By the latest finished run: hidden tests passed, then the time it
    // took; models with none last.
    let key = |m: &CodeBenchModelRuns| {
        m.runs
            .iter()
            .find(|r| r.state == CodeBenchState::Done)
            .map(|r| {
                let passed: u32 = r.tasks.iter().map(|t| t.passed).sum();
                let secs: f64 = r.tasks.iter().filter_map(|t| t.total_secs).sum();
                (std::cmp::Reverse(passed), secs)
            })
    };
    history.sort_by(|a, b| {
        match (key(a), key(b)) {
            (Some(a), Some(b)) => a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)),
            (a, b) => b.is_some().cmp(&a.is_some()),
        }
        .then_with(|| a.model.cmp(&b.model))
    });
    let models = info.map(|info| info.models.len()).unwrap_or(0);
    CodeBenchPage {
        problem,
        run: state.code_bench.run.lock().unwrap().clone().map(with_time),
        history,
        all: state.code_bench.all_progress(),
        models,
        tasks: task_infos(state),
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
/// Why the PC is woken, in the history.
const WAKE_BY: &str = "for the code benchmark";
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
    let key = new_key(state)?;
    // After `active_model`, so the limit comes from the speeds the PC service
    // just reported.
    let answer = post_run(state, &url, &model, &key).await;
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

/// The run's key under Access, its prefix kept so a restart can revoke it.
fn new_key(state: &AppState) -> Result<String, String> {
    let (key, info) = state
        .api_keys
        // opencode works on the model it is told to, under its real name.
        .create(KEY_NAME, ModelName::Show)
        .map_err(|e| match e {
        KeyError::DuplicateName => format!(
            "A key named \"{KEY_NAME}\" exists under Access. Rename it: the code benchmark makes a key of that name for each run."
        ),
        e => e.to_string(),
    })?;
    state.code_bench.set_key(Some(info.prefix));
    Ok(key)
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
    let key = new_key(state)?;
    state.code_bench.set_restore(&restore);
    state.code_bench.cancelled.store(false, Ordering::SeqCst);
    state.code_bench.holding.send_replace(true);
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
    tokio::spawn(async move {
        // Other keys are held until the run ends, also if it panics.
        let _holding = Holding(&state.code_bench.holding);
        run_all(&state, &url, &key).await;
    });
    Ok(())
}

/// Ends the hold of other keys when dropped.
struct Holding<'a>(&'a watch::Sender<bool>);

impl Drop for Holding<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

/// Goes through the models in order, then brings back the one active before the
/// run. A model that fails to load or whose run fails gets its error row and
/// the run goes on; a Cancel stops it after the model that runs now.
async fn run_all(state: &Arc<AppState>, url: &str, key: &str) {
    // A person started it: the PC stays awake to its end, also while the
    // tests run or a model loads and no request goes through.
    let _active = ActivityGuard::new(state);
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
            state
                .code_bench
                .note_failure(id, &e, settings_now(state, id).await);
            continue;
        }
        if state.code_bench.cancelled.load(Ordering::SeqCst) {
            break;
        }
        if let Err(e) = run_model(state, url, id, key).await {
            state
                .code_bench
                .note_failure(id, &e, settings_now(state, id).await);
            continue;
        }
    }
    bring_back(state, &restore).await;
    *state.code_bench.all.lock().unwrap() = None;
    revoke_key(state);
    state.overview.refresh_now();
}

/// Activates the model active before a run over all models again. A failure is
/// noted under Events: the model's own result row stays as it is.
async fn bring_back(state: &Arc<AppState>, model: &str) {
    if let Err(e) = activate(state, model).await {
        state.events.record(
            EventKind::StrataJobFailed,
            Some(format!("Activate {model}: {e}")),
        );
    }
    state.code_bench.clear_restore();
}

/// Activates one model through the PC service, waking the PC first, and waits
/// for the job. Strata's status poll notes the job's end under Events and
/// clears its guard, as for a job started by hand.
async fn activate(state: &Arc<AppState>, model: &str) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err("The PC is not set up.".into());
    }
    if state.ask_pc(&agent).await.is_none() {
        crate::strata::wake(state, WAKE_BY).await?;
    }
    // A job still running, e.g. an Activate from before a proxy restart:
    // the PC service takes no second one, so wait for its end first.
    if agent
        .strata_job()
        .await
        .is_some_and(|job| job.state == StrataJobState::Running)
    {
        let _ = job_end(state, &agent).await;
    }
    let request = StrataJobRequest::Activate {
        model: model.to_string(),
    };
    agent.start_strata_job(&request).await?;
    state.strata.set_job_running(true);
    state.events.record(
        EventKind::StrataJobStarted,
        Some(format!("Activate {model}")),
    );
    job_end(state, &agent).await
}

/// Waits for the PC service's Strata job to end; whether it succeeded.
async fn job_end(state: &AppState, agent: &crate::agent::AgentClient) -> Result<(), String> {
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
            StrataJobState::Done => return Ok(()),
            _ => {
                return Err(job
                    .error
                    .as_deref()
                    .unwrap_or("the model did not load")
                    .to_string());
            }
        }
    }
}

/// Hands the runner the tasks for `model` with the run's key and the
/// model's context; the run as the runner took it.
async fn post_run(
    state: &AppState,
    url: &str,
    model: &str,
    key: &str,
) -> Result<CodeBenchRun, String> {
    let base = timeouts::code_bench_task(state).as_secs();
    let tasks: Vec<_> = TASKS
        .iter()
        .map(|t| {
            let mut task = json!({
                "name": t.name,
                "title": t.title,
                "timeout_secs": base * u64::from(t.limit_factor),
            });
            match t.kind {
                Kind::Files { example, hidden } => {
                    task["files"] = json!({"TASK.md": t.task, "example_test.py": example});
                    task["hidden_test"] = json!(hidden);
                }
                Kind::Repo => {
                    task["kind"] = json!("repo");
                    // The runner's checkout of the same name.
                    task["repo"] = json!(t.name);
                    task["files"] = json!({"TASK.md": t.task});
                }
            }
            task
        })
        .collect();
    // From the PC service's report, so it is known while the PC sleeps;
    // without it the runner leaves opencode's limit out.
    let context = settings_now(state, model).await.and_then(|s| s.context);
    let body = json!({
        "model": model,
        "api_key": key,
        "context": context,
        "tasks": tasks,
        // The default for a task without its own; older runners know only it.
        "timeout_secs": base,
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
    serde_json::from_value::<CodeBenchRun>(value)
        .map_err(|e| format!("The code-bench container answered something unknown: {e}"))
}

/// One model's run inside a run over all models, followed to its end.
async fn run_model(state: &Arc<AppState>, url: &str, model: &str, key: &str) -> Result<(), String> {
    let run = post_run(state, url, model, key).await?;
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
    // No run starts until the model is back and the old key revoked.
    state.code_bench.starting.store(true, Ordering::SeqCst);
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let _starting = Starting(&state.code_bench.starting);
        let _active = ActivityGuard::new(&state);
        if let Some(run) = current(&state, &url).await
            && run.state == CodeBenchState::Running
        {
            *state.code_bench.run.lock().unwrap() = Some(run);
            let was_over_all = state.code_bench.restore_model().is_some();
            follow_run(&state, &url, was_over_all).await;
        }
        if let Some(restore) = state.code_bench.restore_model() {
            bring_back(&state, &restore).await;
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
    // The PC stays awake to the run's end, also while the tests run.
    let _active = ActivityGuard::new(state);
    follow_run(state, url, false).await;
    revoke_key(state);
    state.overview.refresh_now();
}

/// One run to its end. As part of a run over all models (`in_all`) it keeps a
/// failed run as its error row and leaves the shared key for the whole run.
async fn follow_run(state: &Arc<AppState>, url: &str, in_all: bool) {
    let mut missed = 0;
    loop {
        let poll = *state.code_bench.poll.lock().unwrap();
        tokio::time::sleep(poll).await;
        let Some(run) = current(state, url).await else {
            missed += 1;
            if missed < LOST_AFTER {
                continue;
            }
            let lost = {
                let mut current = state.code_bench.run.lock().unwrap();
                current.as_mut().map(|run| {
                    run.state = CodeBenchState::Failed;
                    run.error = Some("The code-bench container stopped answering.".into());
                    run.ended = Some(clock::unix_now());
                    run.clone()
                })
            };
            if let Some(run) = lost {
                state.code_bench.note_failure(
                    &run.model,
                    run.error.as_deref().unwrap_or_default(),
                    settings_now(state, &run.model).await,
                );
            }
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
                let settings = settings_now(state, &run.model).await;
                state.code_bench.keep(run, settings);
                *state.code_bench.run.lock().unwrap() = None;
                break;
            }
            CodeBenchState::Cancelled | CodeBenchState::Failed => {
                if run.state == CodeBenchState::Failed {
                    let why = run.error.clone().unwrap_or_else(|| "the run failed".into());
                    state.code_bench.note_failure(
                        &run.model,
                        &why,
                        settings_now(state, &run.model).await,
                    );
                }
                *state.code_bench.run.lock().unwrap() = Some(run);
                break;
            }
        }
    }
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
