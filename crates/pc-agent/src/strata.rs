//! Strata management for the web UI: what is installed, and jobs that set up,
//! add, switch, delete, update or benchmark models. At most one job runs at a
//! time; it keeps the PC awake and the watchdog away. Every job runs fixed
//! programs with values from [`crate::catalog`]; requests carry no commands.

use std::{
    collections::VecDeque,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    agent::Agent,
    catalog::{self, Size},
    command::{Cancel, CommandSpec},
    managed::Managed,
};

const JOB_FILE: &str = "strata-job.json";
const BENCH_FILE: &str = "strata-benchmark.json";
/// How often the page's visits ask Git whether Strata has a newer version.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(3600);
/// How often a job looks again while it waits for Strata.
const POLL: Duration = Duration::from_millis(250);
/// How long a benchmark waits for Strata to load its model.
const READY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Time for an ended Strata to free its port before the next one starts,
/// as the watchdog gives it.
const STOP_SETTLE: Duration = Duration::from_secs(2);
/// Lines of output a job keeps, for the page to show after a failure.
const KEPT_LINES: usize = 20;
/// Strata's port and network access, as the proxy expects them.
const STRATA_PORT: &str = "8080";
const STRATA_HOST: &str = "0.0.0.0";
/// The engine flag setup writes the context with.
const CONTEXT_FLAG: &str = "--max-context";
/// The engine flag setup loads the speed projection's control vector with.
const PROJECTION_VECTOR_FLAG: &str = "--control-vector-scaled";
/// The flags setup writes after it, with their values.
const PROJECTION_FLAGS: [(&str, &[&str]); 3] = [
    ("--control-vector-layer-range", &["4", "44"]),
    ("--cvec-mode", &["project"]),
    ("--cvec-dir", &["per-layer"]),
];
/// Strata's vector, in the Strata folder.
const PROJECTION_VECTOR: [&str; 3] = [
    "data",
    "experimental-speed-projection",
    "Qwen3.8-Flash-Next-experimental-speed-projection.gguf",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Install,
    AddModel,
    Activate,
    Delete,
    Update,
    Benchmark,
    SetKey,
    SetSpeedProjection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
    /// The service or the PC restarted while it ran.
    Interrupted,
}

/// A job as the NAS sees it. Never holds a key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: JobState,
    /// Setup's step, e.g. "4/7: Downloading the model".
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub done_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub bytes_per_sec: Option<u64>,
    #[serde(default)]
    pub eta_secs: Option<u64>,
    /// The last lines of output.
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// What came out, e.g. a benchmark's tokens per second.
    #[serde(default)]
    pub result: Option<String>,
    pub started: u64,
    #[serde(default)]
    pub ended: Option<u64>,
}

/// A job as the NAS asks for it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobRequest {
    Install {
        family: String,
        size: String,
        context: u32,
        api_key: String,
        /// Strata's experimental speed projection; only for families whose catalog entry offers it.
        #[serde(default)]
        experimental_speed_projection: bool,
    },
    AddModel {
        family: String,
        size: String,
        context: u32,
        /// The key the proxy uses; without it the service's own.
        #[serde(default)]
        api_key: Option<String>,
        /// Strata's experimental speed projection; only for families whose catalog entry offers it.
        #[serde(default)]
        experimental_speed_projection: bool,
    },
    Activate {
        model: String,
    },
    Delete {
        model: String,
    },
    Update,
    Benchmark,
    SetKey {
        api_key: String,
    },
    SetSpeedProjection {
        model: String,
        on: bool,
    },
}

/// Why a job was not started: a value outside the catalog (400), or a state
/// that forbids it now (409).
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    BadRequest(String),
    Conflict(String),
}

/// A checked request: what the job runs with.
enum Plan {
    Install {
        size: &'static Size,
        context: u32,
        api_key: String,
        speed_projection: bool,
    },
    AddModel {
        size: &'static Size,
        context: u32,
        api_key: Option<String>,
        speed_projection: bool,
    },
    Activate {
        id: String,
    },
    Delete {
        id: String,
    },
    Update,
    Benchmark {
        id: String,
    },
    SetKey {
        api_key: String,
    },
    SetSpeedProjection {
        id: String,
        on: bool,
    },
}

impl Plan {
    fn kind(&self) -> JobKind {
        match self {
            Self::Install { .. } => JobKind::Install,
            Self::AddModel { .. } => JobKind::AddModel,
            Self::Activate { .. } => JobKind::Activate,
            Self::Delete { .. } => JobKind::Delete,
            Self::Update => JobKind::Update,
            Self::Benchmark { .. } => JobKind::Benchmark,
            Self::SetKey { .. } => JobKind::SetKey,
            Self::SetSpeedProjection { .. } => JobKind::SetSpeedProjection,
        }
    }

    fn model(&self) -> Option<String> {
        match self {
            Self::Install { size, .. } | Self::AddModel { size, .. } => Some(size.id()),
            Self::Activate { id }
            | Self::Delete { id }
            | Self::Benchmark { id }
            | Self::SetSpeedProjection { id, .. } => Some(id.clone()),
            Self::Update | Self::SetKey { .. } => None,
        }
    }
}

#[derive(Default)]
pub struct Jobs {
    inner: Mutex<JobsInner>,
}

#[derive(Default)]
struct JobsInner {
    current: Option<Job>,
    /// Ends the command the running job waits for.
    cancel: Option<Cancel>,
    cancelled: bool,
    progress: Progress,
    /// The key on the running command's line, kept out of its stored output.
    secret: Option<String>,
    /// A benchmark's chat decode speed, read as its output comes.
    bench: Option<f64>,
}

impl Jobs {
    /// The last job from `dir`; one that was running then is interrupted now.
    pub fn load(dir: &Path) -> Self {
        let current = std::fs::read_to_string(dir.join(JOB_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Job>(&text).ok())
            .map(|mut job| {
                if job.state == JobState::Running {
                    job.state = JobState::Interrupted;
                    job.ended = Some(unix_now());
                }
                job
            });
        Self {
            inner: Mutex::new(JobsInner {
                current,
                ..JobsInner::default()
            }),
        }
    }

    pub fn current(&self) -> Option<Job> {
        self.inner.lock().unwrap().current.clone()
    }

    pub fn running(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|j| j.state == JobState::Running)
    }

    /// Ends the running job; downloaded parts stay for the next start.
    pub fn cancel(&self) -> bool {
        let cancel = {
            let mut inner = self.inner.lock().unwrap();
            if !inner
                .current
                .as_ref()
                .is_some_and(|j| j.state == JobState::Running)
            {
                return false;
            }
            inner.cancelled = true;
            inner.cancel.clone()
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        true
    }

    fn update(&self, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.inner.lock().unwrap().current.as_mut() {
            f(job);
        }
    }
}

/// Starts a job, or says why not.
pub fn start(agent: &Arc<Agent>, request: JobRequest) -> Result<Job, Refusal> {
    // The service ends after an update, and a job with it.
    if agent.updating.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(Refusal::Conflict(
            "Der PC-Dienst wird gerade aktualisiert".into(),
        ));
    }
    let plan = check(agent, request)?;
    if agent.gaming() {
        return Err(Refusal::Conflict(
            "Gaming-Modus ist an: erst Gaming ausschalten".into(),
        ));
    }
    if let Plan::Install { size, .. } | Plan::AddModel { size, .. } = &plan {
        check_space(agent, size)?;
    }
    let job = {
        let mut inner = agent.jobs.inner.lock().unwrap();
        if let Some(running) = inner
            .current
            .as_ref()
            .filter(|j| j.state == JobState::Running)
        {
            return Err(Refusal::Conflict(format!(
                "Es läuft schon ein Vorgang ({})",
                kind_text(running.kind)
            )));
        }
        let id = inner.current.as_ref().map_or(1, |j| j.id + 1);
        let job = Job {
            id,
            kind: plan.kind(),
            model: plan.model(),
            state: JobState::Running,
            step: None,
            percent: None,
            done_bytes: None,
            total_bytes: None,
            bytes_per_sec: None,
            eta_secs: None,
            lines: Vec::new(),
            error: None,
            result: None,
            started: unix_now(),
            ended: None,
        };
        inner.current = Some(job.clone());
        inner.cancel = None;
        inner.cancelled = false;
        inner.progress = Progress::default();
        inner.secret = None;
        inner.bench = None;
        job
    };
    save_job(agent);
    tracing::info!(kind = ?job.kind, model = ?job.model, "Strata-Vorgang gestartet");
    let runner = Arc::clone(agent);
    tokio::spawn(async move {
        let outcome = run(&runner, plan).await;
        finish(&runner, outcome);
    });
    Ok(job)
}

/// Strata's experimental speed projection, only where setup offers it.
fn check_speed_projection(size: &Size, wanted: bool) -> Result<bool, Refusal> {
    if wanted && !takes_speed_projection(size) {
        let offered: Vec<_> = catalog::FAMILIES
            .iter()
            .filter(|f| f.speed_projection)
            .map(|f| f.title)
            .collect();
        return Err(Refusal::BadRequest(format!(
            "Die experimentelle Speed-Projection gibt es nur für {}, nicht für {}",
            offered.join(" und "),
            size.id()
        )));
    }
    Ok(wanted)
}

fn takes_speed_projection(size: &Size) -> bool {
    catalog::family(size.family).is_some_and(|f| f.speed_projection)
}

fn check(agent: &Agent, request: JobRequest) -> Result<Plan, Refusal> {
    let dir = &agent.cfg.strata_dir;
    let size = |family: &str, size: &str| {
        catalog::find(family, size)
            .ok_or_else(|| Refusal::BadRequest(format!("{family} {size} gibt es nicht im Katalog")))
    };
    let context = |n: u32| {
        if catalog::CONTEXTS.contains(&n) {
            Ok(n)
        } else {
            Err(Refusal::BadRequest(format!(
                "Kontextlänge {n} gibt es nicht"
            )))
        }
    };
    match request {
        JobRequest::Install {
            family,
            size: s,
            context: c,
            api_key,
            experimental_speed_projection: wanted,
        } => {
            let size = size(&family, &s)?;
            Ok(Plan::Install {
                size,
                context: context(c)?,
                api_key: check_key(api_key)?,
                speed_projection: check_speed_projection(size, wanted)?,
            })
        }
        JobRequest::AddModel {
            family,
            size: s,
            context: c,
            api_key,
            experimental_speed_projection: wanted,
        } => {
            let size = size(&family, &s)?;
            let plan = Plan::AddModel {
                size,
                context: context(c)?,
                api_key: api_key.map(check_key).transpose()?,
                speed_projection: check_speed_projection(size, wanted)?,
            };
            strata_installed(dir)?;
            Ok(plan)
        }
        JobRequest::Activate { model } => {
            let id = installed_model(dir, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!("{id} ist schon aktiv")));
            }
            Ok(Plan::Activate { id })
        }
        JobRequest::Delete { model } => {
            let id = installed_model(dir, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!(
                    "{id} ist aktiv und lässt sich nicht löschen; erst ein anderes Modell aktivieren"
                )));
            }
            Ok(Plan::Delete { id })
        }
        JobRequest::Update => {
            strata_installed(dir)?;
            if !dir.join(".git").exists() {
                return Err(Refusal::Conflict(format!(
                    "{} ist kein Git-Klon und lässt sich nur von Hand aktualisieren",
                    dir.display()
                )));
            }
            Ok(Plan::Update)
        }
        JobRequest::Benchmark => {
            strata_installed(dir)?;
            let id = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("Kein aktives Modell bekannt; erst ein Modell aktivieren".into())
            })?;
            Ok(Plan::Benchmark { id })
        }
        JobRequest::SetKey { api_key } => {
            let api_key = check_key(api_key)?;
            strata_installed(dir)?;
            Ok(Plan::SetKey { api_key })
        }
        JobRequest::SetSpeedProjection { model, on } => {
            let id = installed_model(dir, &model)?;
            let size = catalog::by_id(&id).expect("installed_model checks the catalog");
            check_speed_projection(size, true)?;
            Ok(Plan::SetSpeedProjection { id, on })
        }
    }
}

fn strata_installed(dir: &Path) -> Result<(), Refusal> {
    if dir.join("setup.py").is_file() {
        Ok(())
    } else {
        Err(Refusal::Conflict(
            "Strata ist noch nicht eingerichtet".into(),
        ))
    }
}

/// The catalog id of an installed model, in the catalog's letter case.
fn installed_model(dir: &Path, model: &str) -> Result<String, Refusal> {
    let id = catalog::by_id(model)
        .ok_or_else(|| Refusal::BadRequest(format!("{model} gibt es nicht im Katalog")))?
        .id();
    if !installed_models(dir).any(|m| m.eq_ignore_ascii_case(&id)) {
        return Err(Refusal::Conflict(format!("{id} ist nicht installiert")));
    }
    Ok(id)
}

/// The model the managed start script starts, in the catalog's letter case.
/// Without a model's own start script (`START-HERE.bat` starts the model
/// used last), the config Strata touched last, as Strata picks it. Without
/// any start script (after the one-line setup) none: the service starts no
/// model, and activating one sets the script.
fn active_model(agent: &Agent) -> Option<String> {
    let managed = agent.managed();
    let script = managed
        .start(&agent.cfg)?
        .0
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let dir = &agent.cfg.strata_dir;
    let id = installed_models(dir)
        .find(|id| run_script(id) == script)
        .or_else(|| {
            installed_models(dir).max_by_key(|id| {
                std::fs::metadata(dir.join(format!("strata-{id}.json")))
                    .and_then(|m| m.modified())
                    .ok()
            })
        })?;
    Some(catalog::by_id(&id).map_or(id, Size::id))
}

/// Keys go into a command line and an HTTP header: printable ASCII only.
fn check_key(key: String) -> Result<String, Refusal> {
    let ok = (16..=256).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
    if ok {
        Ok(key)
    } else {
        Err(Refusal::BadRequest(
            "Der Strata-Key muss 16 bis 256 Zeichen aus Buchstaben, Ziffern, - und _ haben".into(),
        ))
    }
}

/// Refuses a download that does not fit on the data drive. Of a model partly
/// downloaded, only the rest counts.
fn check_space(agent: &Agent, size: &Size) -> Result<(), Refusal> {
    let data = &agent.cfg.strata_data_dir;
    let Some(free) = agent.platform().disk_free(data) else {
        return Ok(());
    };
    let have_gb = dir_size(&data.join("models").join(size.id())) as f64 / 1e9;
    let need_gb = (size.download_gb - have_gb).max(0.0) + catalog::DISK_SPARE_GB;
    let free_gb = free as f64 / 1e9;
    if free_gb < need_gb {
        return Err(Refusal::Conflict(format!(
            "Zu wenig Platz auf dem Laufwerk von {}: {need_gb:.0} GB nötig, {free_gb:.0} GB frei",
            data.display()
        )));
    }
    Ok(())
}

fn partly_downloaded(data: &Path, size: &Size) -> bool {
    std::fs::read_dir(data.join("models").join(size.id()))
        .map(|mut entries| {
            entries.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().contains(".gguf")))
        })
        .unwrap_or(false)
}

enum Outcome {
    Done(Option<String>),
    Failed(String),
    Cancelled,
}

async fn run(agent: &Arc<Agent>, plan: Plan) -> Outcome {
    let result = match plan {
        Plan::Install {
            size,
            context,
            api_key,
            speed_projection,
        } => install(agent, size, context, api_key, speed_projection).await,
        Plan::AddModel {
            size,
            context,
            api_key,
            speed_projection,
        } => add_model(agent, size, context, api_key, speed_projection).await,
        Plan::Activate { id } => activate(agent, &id).await,
        Plan::Delete { id } => delete(agent, &id).await,
        Plan::Update => update(agent).await,
        Plan::Benchmark { id } => benchmark(agent, &id).await,
        Plan::SetKey { api_key } => set_key(agent, api_key).await,
        Plan::SetSpeedProjection { id, on } => set_speed_projection(agent, &id, on).await,
    };
    match result {
        Ok(result) => Outcome::Done(result),
        Err(Step::Cancelled) => Outcome::Cancelled,
        Err(Step::Failed(e)) => Outcome::Failed(e),
    }
}

/// Why a job stopped early.
enum Step {
    Cancelled,
    Failed(String),
}

impl From<String> for Step {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

async fn install(
    agent: &Arc<Agent>,
    size: &Size,
    context: u32,
    api_key: String,
    speed_projection: bool,
) -> Result<Option<String>, Step> {
    let dir = agent.cfg.strata_dir.clone();
    if !dir.join("setup.py").is_file() {
        if dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
            return Err(format!(
                "{} ist nicht leer, aber kein Strata-Ordner; bitte leeren oder strata_dir ändern",
                dir.display()
            )
            .into());
        }
        let git = git(agent).await?;
        set_step(agent, "Strata holen");
        let clone = CommandSpec {
            program: git,
            args: vec![
                "clone".into(),
                agent.cfg.strata_repo.clone(),
                dir.display().to_string(),
            ],
            dir: None,
        };
        command(agent, clone, false).await?;
    }
    setup(agent, size, context, &api_key, speed_projection).await?;
    // A Strata started by hand ends with the old values, the new one starts
    // with the managed ones.
    agent.stop_strata();
    let managed = Managed {
        strata_start: Some(dir.join(run_script(&size.id()))),
        strata_args: Vec::new(),
        strata_processes: Some(vec![folder_entry(&dir)]),
        strata_api_key: Some(api_key),
    };
    agent
        .set_managed(managed)
        .map_err(|e| Step::Failed(e.to_string()))?;
    restart_strata(agent).await;
    Ok(None)
}

async fn add_model(
    agent: &Arc<Agent>,
    size: &Size,
    context: u32,
    api_key: Option<String>,
    speed_projection: bool,
) -> Result<Option<String>, Step> {
    // The proxy's key first: a model with another key would answer it with 401.
    let key = api_key
        .or_else(|| agent.managed().api_key(&agent.cfg).map(String::from))
        .unwrap_or_default();
    setup(agent, size, context, &key, speed_projection).await?;
    Ok(None)
}

/// Waits until Strata has no request at work, so a restart cuts none off.
async fn wait_idle(agent: &Agent) -> Result<(), Step> {
    wait_for(
        agent,
        "Warten, bis Strata die laufende Anfrage beendet hat",
        None,
        || async { !agent.strata_busy().await },
    )
    .await
}

/// Waits until `ready` holds, showing `step` while it does not; ends on
/// cancel and after `limit`.
async fn wait_for<F, Fut>(
    agent: &Agent,
    step: &str,
    limit: Option<Duration>,
    mut ready: F,
) -> Result<(), Step>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let started = Instant::now();
    let mut shown = false;
    loop {
        if agent.jobs.inner.lock().unwrap().cancelled {
            return Err(Step::Cancelled);
        }
        if ready().await {
            return Ok(());
        }
        if !shown {
            set_step(agent, step);
            shown = true;
        }
        if limit.is_some_and(|l| started.elapsed() >= l) {
            return Err(format!(
                "{step}: nach {} min aufgegeben",
                started.elapsed().as_secs() / 60
            )
            .into());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Starts Strata again after a job ended it, unless gaming mode is on.
async fn restart_strata(agent: &Agent) {
    tokio::time::sleep(STOP_SETTLE).await;
    if !agent.gaming() {
        agent.start_strata();
    }
}

async fn activate(agent: &Arc<Agent>, id: &str) -> Result<Option<String>, Step> {
    wait_idle(agent).await?;
    set_step(agent, "Strata mit dem neuen Modell starten");
    agent.stop_strata();
    let dir = &agent.cfg.strata_dir;
    let mut managed = agent.managed();
    managed.strata_start = Some(dir.join(run_script(id)));
    managed.strata_args = Vec::new();
    managed
        .strata_processes
        .get_or_insert_with(|| vec![folder_entry(dir)]);
    let saved = agent
        .set_managed(managed)
        .map_err(|e| Step::Failed(e.to_string()));
    // Also when saving failed: Strata runs again, with the old model then.
    restart_strata(agent).await;
    saved?;
    Ok(None)
}

/// What setup leaves behind for a model: its folders in the data folder,
/// then its config, start script and log in the Strata folder. Files other
/// models share (`mtp`) stay.
fn model_paths(cfg: &crate::Config, id: &str) -> Vec<PathBuf> {
    let lower = id.to_lowercase();
    let data = &cfg.strata_data_dir;
    let dir = &cfg.strata_dir;
    vec![
        data.join("models").join(id),
        data.join("packs").join(&lower),
        dir.join(format!("strata-{lower}.log")),
        dir.join(run_script(id)),
        // The config last: until it is gone, the model is listed and can be deleted again.
        dir.join(format!("strata-{lower}.json")),
    ]
}

async fn delete(agent: &Arc<Agent>, id: &str) -> Result<Option<String>, Step> {
    set_step(agent, "Dateien löschen");
    let paths = model_paths(&agent.cfg, id);
    tokio::task::spawn_blocking(move || {
        paths.iter().try_for_each(|path| {
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match removed {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("{} nicht gelöscht: {e}", path.display()))
                }
                _ => Ok(()),
            }
        })
    })
    .await
    .map_err(|e| Step::Failed(e.to_string()))??;
    Ok(None)
}

/// Like Strata's `UPDATE.bat`: the newest code, then setup's `--update`
/// with Strata ended, so the engine can be replaced. Model files stay.
async fn update(agent: &Arc<Agent>) -> Result<Option<String>, Step> {
    let dir = agent.cfg.strata_dir.clone();
    let git = git(agent).await?;
    set_step(agent, "Neue Version holen");
    let pull = CommandSpec {
        program: git,
        args: vec!["pull".into(), "--ff-only".into()],
        dir: Some(dir.clone()),
    };
    command(agent, pull, false).await?;
    wait_idle(agent).await?;
    agent.stop_strata();
    let spec = CommandSpec {
        program: dir.join("START-HERE.bat"),
        args: vec!["--update".into()],
        dir: Some(dir),
    };
    let updated = command(agent, spec, true).await;
    restart_strata(agent).await;
    updated?;
    agent.updates.found(Some(false));
    Ok(None)
}

/// The last benchmark run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Benchmark {
    pub model: String,
    /// Tokens per second while answering, e.g. `38.5 Token/s`.
    pub result: String,
    /// Unix seconds.
    pub at: u64,
}

/// Runs Strata's `BENCH.bat` against the running Strata and keeps its chat
/// decode speed.
async fn benchmark(agent: &Arc<Agent>, id: &str) -> Result<Option<String>, Step> {
    // A Strata that is still loading gets no second start; one that hangs
    // ends first.
    if !agent.strata_healthy().await && !agent.strata_loading() {
        agent.stop_strata();
        restart_strata(agent).await;
    }
    wait_for(
        agent,
        "Warten, bis Strata bereit ist",
        Some(READY_TIMEOUT),
        || agent.strata_healthy(),
    )
    .await?;
    set_step(agent, "Benchmark läuft (etwa 10 min)");
    let dir = agent.cfg.strata_dir.clone();
    let config = config_path(&dir, id);
    let spec = CommandSpec {
        program: dir.join("BENCH.bat"),
        args: vec!["--config".into(), config.display().to_string()],
        dir: Some(dir),
    };
    command(agent, spec, false).await?;
    let tps = agent
        .jobs
        .inner
        .lock()
        .unwrap()
        .bench
        .ok_or_else(|| "Der Benchmark hat keine Tokens/s gemeldet".to_string())?;
    let result = format!("{} Token/s", (tps * 10.0).round() / 10.0);
    let record = Benchmark {
        model: id.to_string(),
        result: result.clone(),
        at: unix_now(),
    };
    let path = agent.cfg.state_dir().join(BENCH_FILE);
    let text = serde_json::to_string_pretty(&record).expect("benchmark serializes");
    if let Err(e) = crate::write_replacing(&path, &text) {
        tracing::warn!("{BENCH_FILE} nicht gespeichert: {e}");
    }
    Ok(Some(result))
}

/// `  chat decode tok/s     38.50   (37.10 - 39.20, n=3)` → 38.5, from the
/// short table `tools/strata_bench.py` prints at the end.
fn parse_bench(line: &str) -> Option<f64> {
    line.trim()
        .strip_prefix("chat decode tok/s")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn load_benchmark(agent: &Agent) -> Option<Benchmark> {
    let text = std::fs::read_to_string(agent.cfg.state_dir().join(BENCH_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Puts the key into every model's config and restarts Strata with it.
async fn set_key(agent: &Arc<Agent>, api_key: String) -> Result<Option<String>, Step> {
    wait_idle(agent).await?;
    set_step(agent, "Key eintragen");
    let dir = agent.cfg.strata_dir.clone();
    let key = api_key.clone();
    tokio::task::spawn_blocking(move || write_key(&dir, &key))
        .await
        .map_err(|e| Step::Failed(e.to_string()))??;
    let mut managed = agent.managed();
    managed.strata_api_key = Some(api_key);
    agent
        .set_managed(managed)
        .map_err(|e| Step::Failed(e.to_string()))?;
    set_step(agent, "Strata neu starten");
    agent.stop_strata();
    restart_strata(agent).await;
    Ok(None)
}

/// Sets `api_key` in each model config, keeping everything else.
fn write_key(dir: &Path, key: &str) -> Result<(), String> {
    // Every config is read first: one that does not parse stops the change
    // before any has the new key.
    let mut changed = Vec::new();
    for id in installed_models(dir) {
        let path = dir.join(format!("strata-{id}.json"));
        let Some((text, mut cfg)) = read_config(&path)? else {
            continue;
        };
        cfg.insert("api_key".into(), key.into());
        changed.push((path, text, config_text(cfg)));
    }
    for (i, (path, _, new)) in changed.iter().enumerate() {
        if let Err(e) = crate::write_replacing(path, new) {
            // Back to the old key, so all configs agree with the service.
            for (path, old, _) in &changed[..i] {
                let _ = crate::write_replacing(path, old);
            }
            return Err(format!("{} nicht geschrieben: {e}", path.display()));
        }
    }
    Ok(())
}

/// Switches the speed projection in a model's config, with the flags setup
/// writes for it; everything else in the config stays. The active model
/// restarts with it once Strata has no request at work.
async fn set_speed_projection(
    agent: &Arc<Agent>,
    id: &str,
    on: bool,
) -> Result<Option<String>, Step> {
    set_step(agent, "Konfiguration ändern");
    let dir = agent.cfg.strata_dir.clone();
    let model = id.to_string();
    tokio::task::spawn_blocking(move || write_speed_projection(&dir, &model, on))
        .await
        .map_err(|e| Step::Failed(e.to_string()))??;
    // The engine reads its flags at the start: the active model starts again.
    if active_model(agent).as_deref() == Some(id) {
        wait_idle(agent).await?;
        set_step(agent, "Strata neu starten");
        agent.stop_strata();
        restart_strata(agent).await;
    }
    Ok(None)
}

fn write_speed_projection(dir: &Path, id: &str, on: bool) -> Result<(), String> {
    let path = config_path(dir, id);
    let (_, mut cfg) = read_config(&path)?
        .ok_or_else(|| format!("{} ist keine Modell-Konfiguration", path.display()))?;
    let vector: PathBuf = PROJECTION_VECTOR
        .iter()
        .fold(dir.to_path_buf(), |p, part| p.join(part));
    if on && !vector.is_file() {
        return Err(format!(
            "Der Steuervektor der Speed-Projection fehlt: {}",
            vector.display()
        ));
    }
    let args = cfg["args"].as_array_mut().expect("read_config checks args");
    without_speed_projection(args);
    if on {
        args.push(PROJECTION_VECTOR_FLAG.into());
        args.push(format!("{}:1.0", vector.display()).into());
        for (flag, values) in PROJECTION_FLAGS {
            args.push(flag.into());
            args.extend(values.iter().map(|&v| serde_json::Value::from(v)));
        }
    }
    // Without its own start script Strata starts the config touched last:
    // the change keeps the time, so the same model stays active.
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    crate::write_replacing(&path, &config_text(cfg))
        .map_err(|e| format!("{} nicht geschrieben: {e}", path.display()))?;
    if let Some(time) = modified {
        let kept = std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(time));
        if let Err(e) = kept {
            tracing::warn!("Änderungszeit von {} nicht behalten: {e}", path.display());
        }
    }
    Ok(())
}

/// Takes setup's speed projection flags out of `args`, each with its values,
/// whatever they are.
fn without_speed_projection(args: &mut Vec<serde_json::Value>) {
    let mut kept = Vec::with_capacity(args.len());
    let mut values = 0;
    for arg in args.drain(..) {
        if let Some(n) = arg.as_str().and_then(projection_flag_values) {
            values = n;
        } else if values > 0 && !arg.as_str().is_some_and(|a| a.starts_with("--")) {
            values -= 1;
        } else {
            values = 0;
            kept.push(arg);
        }
    }
    *args = kept;
}

/// How many values one of setup's speed projection flags takes.
fn projection_flag_values(flag: &str) -> Option<usize> {
    if flag == PROJECTION_VECTOR_FLAG {
        return Some(1);
    }
    PROJECTION_FLAGS
        .iter()
        .find(|(f, _)| *f == flag)
        .map(|(_, values)| values.len())
}

/// `strata-<id>.json`, as setup names it.
fn config_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("strata-{}.json", id.to_lowercase()))
}

/// A model config with its text as read, or `None` for a `strata-*.json`
/// that is none: model configs have `exe` and an `args` array, as Strata's
/// own tools tell them apart.
fn read_config(path: &Path) -> Result<Option<(String, ConfigFields)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} nicht lesbar: {e}", path.display()))?;
    let cfg = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{} ist kein gültiges JSON: {e}", path.display()))?;
    let serde_json::Value::Object(fields) = cfg else {
        return Ok(None);
    };
    if !fields.contains_key("exe") || !fields.get("args").is_some_and(|a| a.is_array()) {
        return Ok(None);
    }
    Ok(Some((text, fields)))
}

type ConfigFields = serde_json::Map<String, serde_json::Value>;

/// A config as Strata writes it (`json.dumps(cfg, indent=1)`).
fn config_text(cfg: ConfigFields) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(&cfg, &mut ser).expect("config serializes");
    String::from_utf8(out).expect("JSON is UTF-8")
}

/// Whether Strata's Git source has commits this folder lacks; asked in the
/// background at most once an hour, when the page is looked at.
#[derive(Default)]
pub struct UpdateCheck {
    inner: Mutex<UpdateState>,
}

#[derive(Default)]
struct UpdateState {
    checked: Option<Instant>,
    asking: bool,
    available: Option<bool>,
}

impl UpdateCheck {
    pub fn available(&self) -> Option<bool> {
        self.inner.lock().unwrap().available
    }

    /// Claims the next check, if one is due.
    fn claim(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        if state.asking
            || state
                .checked
                .is_some_and(|t| t.elapsed() < UPDATE_CHECK_EVERY)
        {
            return false;
        }
        state.asking = true;
        true
    }

    fn found(&self, available: Option<bool>) {
        let mut state = self.inner.lock().unwrap();
        state.asking = false;
        state.checked = Some(Instant::now());
        state.available = available;
    }
}

/// Asks Git in the background whether an update is available, if it is
/// time to; the page shows the answer at its next visit.
pub fn check_for_update(agent: &Arc<Agent>) {
    if !agent.cfg.strata_dir.join(".git").exists() || agent.jobs.running() || !agent.updates.claim()
    {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let available = commits_behind(&agent).await.map(|n| n > 0);
        agent.updates.found(available);
    });
}

async fn commits_behind(agent: &Agent) -> Option<u64> {
    let git = find_git(agent).await?;
    let dir = agent.cfg.strata_dir.clone();
    let run = |args: &[&str]| CommandSpec {
        program: git.clone(),
        args: args.iter().map(|a| a.to_string()).collect(),
        dir: Some(dir.clone()),
    };
    let (code, _) = output(agent, &run(&["fetch", "--quiet"])).await?;
    if code != 0 {
        return None;
    }
    let (code, lines) = output(agent, &run(&["rev-list", "--count", "HEAD..@{u}"])).await?;
    if code != 0 {
        return None;
    }
    lines.iter().find_map(|l| l.trim().parse().ok())
}

/// Runs Strata's setup without questions, with the fixed values and the key.
async fn setup(
    agent: &Arc<Agent>,
    size: &Size,
    context: u32,
    api_key: &str,
    speed_projection: bool,
) -> Result<(), Step> {
    let dir = &agent.cfg.strata_dir;
    let mut args: Vec<String> = vec!["--yes".into(), "--no-start".into(), "--no-browser".into()];
    if installed_models(dir).next().is_some() {
        args.push("--setup".into());
    }
    args.extend(
        [
            "--family",
            size.family,
            "--model",
            size.size,
            "--context",
            &context.to_string(),
            "--vision",
            "no",
            "--host",
            STRATA_HOST,
            "--port",
            STRATA_PORT,
            "--data-dir",
            &agent.cfg.strata_data_dir.display().to_string(),
        ]
        .map(String::from),
    );
    if !api_key.is_empty() {
        args.extend(["--api-key".into(), api_key.to_string()]);
        agent.jobs.inner.lock().unwrap().secret = Some(api_key.to_string());
    }
    if takes_speed_projection(size) {
        // Always explicit: otherwise setup takes it over from an earlier install.
        let value = if speed_projection { "on" } else { "off" };
        args.extend(["--experimental-speed-projection".into(), value.into()]);
    }
    let spec = CommandSpec {
        program: dir.join("START-HERE.bat"),
        args,
        dir: Some(dir.clone()),
    };
    command(agent, spec, true).await
}

/// Git, installed with winget for this user if it is missing.
async fn git(agent: &Arc<Agent>) -> Result<PathBuf, Step> {
    if let Some(git) = find_git(agent).await {
        return Ok(git);
    }
    set_step(agent, "Git installieren");
    let winget = CommandSpec {
        program: "winget".into(),
        args: [
            "install",
            "--id",
            "Git.Git",
            "-e",
            "--scope",
            "user",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ]
        .map(String::from)
        .to_vec(),
        dir: None,
    };
    command(agent, winget, false).await.map_err(|e| match e {
        Step::Failed(e) => Step::Failed(format!(
            "Git fehlt und ließ sich nicht installieren ({e}); bitte Git für Windows installieren"
        )),
        cancelled => cancelled,
    })?;
    find_git(agent).await.ok_or_else(|| {
        "Git wurde installiert, ist aber nicht zu finden; bitte den PC-Dienst neu starten"
            .to_string()
            .into()
    })
}

/// Git on PATH, or where winget puts it: a Git just installed is not on
/// this process's PATH yet.
async fn find_git(agent: &Agent) -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("git")];
    for var in ["LOCALAPPDATA", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            candidates.push(
                base.join("Programs")
                    .join("Git")
                    .join("cmd")
                    .join("git.exe"),
            );
            candidates.push(base.join("Git").join("cmd").join("git.exe"));
        }
    }
    for candidate in candidates {
        let version = CommandSpec {
            program: candidate.clone(),
            args: vec!["--version".into()],
            dir: None,
        };
        if output(agent, &version)
            .await
            .is_some_and(|(code, _)| code == 0)
        {
            return Some(candidate);
        }
    }
    None
}

/// Runs a command outside a job: its exit code and output, `None` if it
/// could not run or was ended.
async fn output(agent: &Agent, spec: &CommandSpec) -> Option<(i32, Vec<String>)> {
    let mut run = agent.platform().run(spec).ok()?;
    let mut lines = Vec::new();
    while let Some(line) = run.next_line().await {
        lines.push(line);
    }
    Some((run.wait().await?, lines))
}

/// Runs one command of a job: keeps its output, reads setup's progress
/// when `progress` is set, and ends it on cancel.
async fn command(agent: &Arc<Agent>, spec: CommandSpec, progress: bool) -> Result<(), Step> {
    let mut run = agent
        .platform()
        .run(&spec)
        .map_err(|e| Step::Failed(e.to_string()))?;
    {
        let mut inner = agent.jobs.inner.lock().unwrap();
        if inner.cancelled {
            drop(inner);
            run.canceller()();
            return Err(Step::Cancelled);
        }
        inner.cancel = Some(run.canceller());
    }
    while let Some(line) = run.next_line().await {
        let step_changed = {
            let mut inner = agent.jobs.inner.lock().unwrap();
            let JobsInner {
                current,
                progress: p,
                secret,
                bench,
                ..
            } = &mut *inner;
            let line = match secret.as_deref() {
                Some(key) => line.replace(key, "***"),
                None => line,
            };
            if let Some(tps) = parse_bench(&line) {
                *bench = Some(tps);
            }
            let Some(job) = current.as_mut() else {
                continue;
            };
            let before = job.step.clone();
            if progress {
                p.read(&line, job);
            }
            let mut lines: VecDeque<String> = std::mem::take(&mut job.lines).into();
            lines.push_back(line);
            while lines.len() > KEPT_LINES {
                lines.pop_front();
            }
            job.lines = lines.into();
            job.step != before
        };
        if step_changed {
            save_job(agent);
        }
    }
    let code = run.wait().await;
    let cancelled = {
        let mut inner = agent.jobs.inner.lock().unwrap();
        inner.cancel = None;
        inner.cancelled
    };
    if cancelled {
        return Err(Step::Cancelled);
    }
    match code {
        Some(0) => Ok(()),
        code => {
            let error = agent.jobs.current().and_then(|j| j.error);
            let program = spec.program.file_name().map_or_else(
                || spec.program.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let ended = match code {
                Some(code) => format!("{program} endete mit Code {code}"),
                None => format!("{program} wurde beendet"),
            };
            Err(Step::Failed(match error {
                Some(error) => format!("{ended}: {error}"),
                None => ended,
            }))
        }
    }
}

fn set_step(agent: &Agent, step: &str) {
    agent.jobs.update(|job| {
        job.step = Some(step.to_string());
        job.percent = None;
    });
    save_job(agent);
}

fn finish(agent: &Agent, outcome: Outcome) {
    agent.jobs.update(|job| {
        job.ended = Some(unix_now());
        job.bytes_per_sec = None;
        job.eta_secs = None;
        match outcome {
            Outcome::Done(result) => {
                job.state = JobState::Done;
                job.result = result;
                job.percent = Some(100);
            }
            Outcome::Failed(error) => {
                job.state = JobState::Failed;
                job.error = Some(error);
            }
            Outcome::Cancelled => job.state = JobState::Cancelled,
        }
    });
    save_job(agent);
    if let Some(job) = agent.jobs.current() {
        tracing::info!(kind = ?job.kind, state = ?job.state, error = ?job.error, "Strata-Vorgang beendet");
    }
}

fn save_job(agent: &Agent) {
    let Some(job) = agent.jobs.current() else {
        return;
    };
    let text = serde_json::to_string_pretty(&job).expect("job serializes");
    if let Err(e) = crate::write_replacing(&agent.cfg.state_dir().join(JOB_FILE), &text) {
        tracing::warn!("{JOB_FILE} nicht gespeichert: {e}");
    }
}

/// Reads setup's output: the step, a download's progress, an error. The
/// same lines Strata's own MCP tool reads (`tools/strata_mcp.py`).
#[derive(Default)]
struct Progress {
    /// Bytes at the last speed measurement.
    last: Option<(Instant, u64)>,
}

impl Progress {
    fn read(&mut self, line: &str, job: &mut Job) {
        if let Some(step) = parse_step(line) {
            job.step = Some(step);
            job.percent = None;
            job.done_bytes = None;
            job.total_bytes = None;
            job.bytes_per_sec = None;
            job.eta_secs = None;
            self.last = None;
            return;
        }
        if let Some((done, total, percent)) = parse_download(line) {
            job.percent = Some(percent);
            job.done_bytes = Some(done);
            job.total_bytes = Some(total);
            let now = Instant::now();
            match self.last {
                Some((at, bytes)) if now.duration_since(at) >= Duration::from_secs(1) => {
                    let secs = now.duration_since(at).as_secs_f64();
                    let speed = (done.saturating_sub(bytes) as f64 / secs) as u64;
                    job.bytes_per_sec = Some(speed);
                    job.eta_secs = (speed > 0).then(|| total.saturating_sub(done) / speed);
                    self.last = Some((now, done));
                }
                Some(_) => {}
                None => self.last = Some((now, done)),
            }
            return;
        }
        if let Some(error) = line.trim().strip_prefix("[X]") {
            job.error = Some(error.trim().to_string());
        }
    }
}

/// `=== Step 4: Downloading the model ===` → `4/7: Downloading the model`.
fn parse_step(line: &str) -> Option<String> {
    let rest = line
        .trim()
        .strip_prefix("=== Step ")?
        .strip_suffix(" ===")?;
    let (n, title) = rest.split_once(": ")?;
    n.parse::<u32>().ok()?;
    Some(format!("{n}/7: {title}"))
}

/// `  model.gguf: 12.3 / 68.0 GB (18%)` → bytes done, bytes in total, percent.
fn parse_download(line: &str) -> Option<(u64, u64, u8)> {
    let (_, rest) = line.trim().rsplit_once(": ")?;
    let (amounts, percent) = rest.trim().strip_suffix("%)")?.split_once(" GB (")?;
    let (done, total) = amounts.split_once(" / ")?;
    let gb = |s: &str| s.trim().parse::<f64>().ok().map(|v| (v * 1e9) as u64);
    Some((gb(done)?, gb(total)?, percent.trim().parse().ok()?))
}

/// Ids of the models set up in `dir`, from their `strata-<id>.json`.
pub fn installed_models(dir: &Path) -> impl Iterator<Item = String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_prefix("strata-")?.strip_suffix(".json")?;
            // `strata-<model>.shared-settings.json` belongs to a model.
            (!id.is_empty() && !id.contains('.')).then(|| id.to_string())
        })
}

/// `run-<id>.bat`, as setup names it.
pub fn run_script(id: &str) -> String {
    format!("run-{}.bat", id.to_lowercase())
}

/// The folder as a `strata_processes` entry.
fn folder_entry(dir: &Path) -> String {
    let mut entry = dir.display().to_string();
    if !crate::platform::is_folder(&entry) {
        entry.push(std::path::MAIN_SEPARATOR);
    }
    entry
}

fn kind_text(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Install => "Einrichtung",
        JobKind::AddModel => "Modell hinzufügen",
        JobKind::Activate => "Modell aktivieren",
        JobKind::Delete => "Modell löschen",
        JobKind::Update => "Update",
        JobKind::Benchmark => "Benchmark",
        JobKind::SetKey => "Strata-Key setzen",
        JobKind::SetSpeedProjection => "Speed-Projection umschalten",
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the page shows about Strata on this PC.
#[derive(Debug, Serialize, Deserialize)]
pub struct StrataView {
    pub installed: bool,
    /// Short commit of the Strata folder.
    pub version: Option<String>,
    /// Whether Strata's source has newer commits; `None` until known.
    pub update_available: Option<bool>,
    pub models: Vec<InstalledModel>,
    pub families: Vec<FamilyView>,
    pub catalog: Vec<CatalogEntry>,
    pub recommended: Option<String>,
    pub contexts: Vec<u32>,
    pub default_context: u32,
    pub ram_gb: f64,
    pub disk_free_gb: Option<f64>,
    pub data_dir: String,
    pub benchmark: Option<Benchmark>,
    pub job: Option<Job>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FamilyView {
    pub id: String,
    pub title: String,
    /// Setup offers the experimental speed projection.
    #[serde(default)]
    pub speed_projection: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub family: Option<String>,
    pub size: Option<String>,
    pub disk_bytes: u64,
    pub active: bool,
    /// Its config loads a control vector, Strata's or another one.
    pub speed_projection: bool,
    /// The context it starts with (`--max-context`); `None` if its config
    /// does not say.
    pub context: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Model id, e.g. `coder-IQ1_M`.
    pub id: String,
    pub family: String,
    pub size: String,
    pub download_gb: f64,
    pub ram_gb: u32,
    /// The PC has the RAM for it.
    pub fits: bool,
    /// Parts are downloaded already; a start continues there.
    pub partly_downloaded: bool,
}

pub fn view(agent: &Agent) -> StrataView {
    let cfg = &agent.cfg;
    let dir = &cfg.strata_dir;
    let ram_gb = agent.platform().ram_bytes() as f64 / (1u64 << 30) as f64;
    let active = active_model(agent);
    let mut models: Vec<InstalledModel> = installed_models(dir)
        .map(|id| {
            let size = catalog::by_id(&id);
            let id = size.map_or(id, |s| s.id());
            InstalledModel {
                disk_bytes: model_paths(cfg, &id)[..2].iter().map(|p| dir_size(p)).sum(),
                active: active.as_deref() == Some(id.as_str()),
                speed_projection: has_speed_projection(dir, &id),
                context: model_context(dir, &id),
                family: size.map(|s| s.family.to_string()),
                size: size.map(|s| s.size.to_string()),
                id,
            }
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    StrataView {
        installed: dir.join("setup.py").is_file(),
        version: git_commit(dir),
        update_available: agent.updates.available(),
        models,
        families: catalog::FAMILIES
            .iter()
            .map(|f| FamilyView {
                id: f.id.into(),
                title: f.title.into(),
                speed_projection: f.speed_projection,
            })
            .collect(),
        catalog: catalog::SIZES
            .iter()
            .map(|s| CatalogEntry {
                id: s.id(),
                family: s.family.into(),
                size: s.size.into(),
                download_gb: s.download_gb,
                ram_gb: s.ram_gb,
                fits: ram_gb == 0.0 || f64::from(s.ram_gb) <= ram_gb + 0.5,
                partly_downloaded: partly_downloaded(&cfg.strata_data_dir, s),
            })
            .collect(),
        recommended: catalog::recommend(ram_gb).map(Size::id),
        contexts: catalog::CONTEXTS.to_vec(),
        default_context: catalog::DEFAULT_CONTEXT,
        ram_gb,
        disk_free_gb: agent
            .platform()
            .disk_free(&cfg.strata_data_dir)
            .map(|b| b as f64 / 1e9),
        data_dir: cfg.strata_data_dir.display().to_string(),
        benchmark: load_benchmark(agent),
        job: agent.jobs.current(),
    }
}

/// Whether a model's config loads a control vector; a config that cannot
/// be read has none.
fn has_speed_projection(dir: &Path, id: &str) -> bool {
    read_config(&config_path(dir, id))
        .ok()
        .flatten()
        .is_some_and(|(_, cfg)| {
            cfg["args"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == PROJECTION_VECTOR_FLAG))
        })
}

/// The value of `--max-context` in a model's config.
fn model_context(dir: &Path, id: &str) -> Option<u32> {
    let (_, cfg) = read_config(&config_path(dir, id)).ok().flatten()?;
    let args = cfg["args"].as_array()?;
    let at = args.iter().position(|v| v == CONTEXT_FLAG)?;
    args.get(at + 1)?.as_str()?.parse().ok()
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

/// The checked-out commit, read from `.git` without running Git.
fn git_commit(dir: &Path) -> Option<String> {
    let git = dir.join(".git");
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let full = match head.strip_prefix("ref: ") {
        None => head.to_string(),
        Some(name) => std::fs::read_to_string(git.join(name))
            .ok()
            .or_else(|| {
                let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
                packed
                    .lines()
                    .find(|l| l.ends_with(&format!(" {name}")))
                    .map(|l| l.split(' ').next().unwrap_or_default().to_string())
            })?
            .trim()
            .to_string(),
    };
    (full.len() >= 7).then(|| full[..7].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_setup_steps_and_downloads() {
        assert_eq!(
            parse_step("=== Step 4: Downloading the model ===").as_deref(),
            Some("4/7: Downloading the model")
        );
        assert_eq!(
            parse_download(
                "  Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf: 12.5 / 50.0 GB (25%)"
            ),
            Some((12_500_000_000, 50_000_000_000, 25))
        );
        assert_eq!(parse_download("nothing: here"), None);
    }
}
