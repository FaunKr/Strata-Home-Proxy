//! Strata management for the web UI: what is installed, and jobs that set up,
//! add, switch, delete, update, benchmark or compare models. At most one job runs at a
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
    install::{Installation, Model},
    managed::Managed,
    sampling::ModelSampling,
};

const JOB_FILE: &str = "strata-job.json";
/// The last benchmark run, as older services kept it alone.
const BENCH_FILE: &str = "strata-benchmark.json";
/// The last benchmark of each model.
const BENCHES_FILE: &str = "strata-benchmarks.json";
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
/// The longest context the switch takes: the model's trained length, which
/// needs no rope scaling.
const MAX_SWITCHED_CONTEXT: u32 = 262_144;
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
    SetContext,
    CompareModels,
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
    SetContext {
        model: String,
        context: u32,
    },
    CompareModels,
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
    SetContext {
        id: String,
        context: u32,
    },
    /// Benchmarks `ids` in turn, then `active` is the active model again.
    CompareModels {
        ids: Vec<String>,
        active: String,
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
            Self::SetContext { .. } => JobKind::SetContext,
            Self::CompareModels { .. } => JobKind::CompareModels,
        }
    }

    fn model(&self) -> Option<String> {
        match self {
            Self::Install { size, .. } | Self::AddModel { size, .. } => Some(size.id()),
            Self::Activate { id }
            | Self::Delete { id }
            | Self::Benchmark { id }
            | Self::SetSpeedProjection { id, .. }
            | Self::SetContext { id, .. } => Some(id.clone()),
            Self::Update | Self::SetKey { .. } | Self::CompareModels { .. } => None,
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
    /// A benchmark's speeds, read as its output comes.
    bench: BenchReading,
    /// Goes before each step, e.g. which model of a comparison.
    step_prefix: Option<String>,
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
        inner.bench = BenchReading::default();
        inner.step_prefix = None;
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
    let install = Installation::of(&agent.cfg);
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
            strata_installed(&install)?;
            Ok(plan)
        }
        JobRequest::Activate { model } => {
            let id = installed_model(&install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!("{id} ist schon aktiv")));
            }
            Ok(Plan::Activate { id })
        }
        JobRequest::Delete { model } => {
            let id = installed_model(&install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!(
                    "{id} ist aktiv und lässt sich nicht löschen; erst ein anderes Modell aktivieren"
                )));
            }
            Ok(Plan::Delete { id })
        }
        JobRequest::Update => {
            strata_installed(&install)?;
            if !dir.join(".git").exists() {
                return Err(Refusal::Conflict(format!(
                    "{} ist kein Git-Klon und lässt sich nur von Hand aktualisieren",
                    dir.display()
                )));
            }
            Ok(Plan::Update)
        }
        JobRequest::Benchmark => {
            strata_installed(&install)?;
            let id = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("Kein aktives Modell bekannt; erst ein Modell aktivieren".into())
            })?;
            Ok(Plan::Benchmark { id })
        }
        JobRequest::SetKey { api_key } => {
            let api_key = check_key(api_key)?;
            strata_installed(&install)?;
            Ok(Plan::SetKey { api_key })
        }
        JobRequest::SetSpeedProjection { model, on } => {
            let id = installed_model(&install, &model)?;
            let size = catalog::by_id(&id).expect("installed_model checks the catalog");
            check_speed_projection(size, true)?;
            Ok(Plan::SetSpeedProjection { id, on })
        }
        JobRequest::SetContext { model, context: c } => {
            let context = context(c)?;
            if context > MAX_SWITCHED_CONTEXT {
                return Err(Refusal::BadRequest(format!(
                    "Kontextlänge {context} braucht Rope-Scaling und geht nur über Setup"
                )));
            }
            let id = installed_model(&install, &model)?;
            Ok(Plan::SetContext { id, context })
        }
        JobRequest::CompareModels => {
            strata_installed(&install)?;
            let active = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("Kein aktives Modell bekannt; erst ein Modell aktivieren".into())
            })?;
            let mut ids: Vec<String> = install
                .models()
                .into_iter()
                .map(|id| catalog::by_id(&id).map_or(id, Size::id))
                .collect();
            ids.sort();
            Ok(Plan::CompareModels { ids, active })
        }
    }
}

fn strata_installed(install: &Installation) -> Result<(), Refusal> {
    if install.is_set_up() {
        Ok(())
    } else {
        Err(Refusal::Conflict(
            "Strata ist noch nicht eingerichtet".into(),
        ))
    }
}

/// The catalog id of an installed model, in the catalog's letter case.
fn installed_model(install: &Installation, model: &str) -> Result<String, Refusal> {
    let id = catalog::by_id(model)
        .ok_or_else(|| Refusal::BadRequest(format!("{model} gibt es nicht im Katalog")))?
        .id();
    if !install.has(&id) {
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
    let (script, _) = managed.start(&agent.cfg)?;
    let id = Installation::of(&agent.cfg).active_model(script)?;
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
    let have_gb = Installation::of(&agent.cfg).downloaded_bytes(&size.id()) as f64 / 1e9;
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
        Plan::SetContext { id, context } => set_context(agent, &id, context).await,
        Plan::CompareModels { ids, active } => compare_models(agent, &ids, &active).await,
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
    let strata = Installation::of(&agent.cfg);
    if !strata.is_set_up() {
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
    manage(agent).await;
    let managed = Managed {
        strata_start: Some(strata.run_script(&size.id())),
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
    if active_changed(agent, &manage(agent).await) {
        wait_idle(agent).await?;
        set_step(agent, "Strata neu starten");
        agent.stop_strata();
        restart_strata(agent).await;
    }
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
    managed.strata_start = Some(Installation::of(&agent.cfg).run_script(id));
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

async fn delete(agent: &Arc<Agent>, id: &str) -> Result<Option<String>, Step> {
    set_step(agent, "Dateien löschen");
    let install = Installation::of(&agent.cfg);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || install.remove(&id))
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
    if updated.is_ok() {
        manage(agent).await;
    }
    restart_strata(agent).await;
    updated?;
    agent.updates.found(Some(false), None);
    Ok(None)
}

/// A model's last benchmark run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Benchmark {
    pub model: String,
    /// Tokens per second while answering, e.g. `38.5 Token/s`; empty when
    /// the run failed.
    pub result: String,
    /// Unix seconds.
    pub at: u64,
    /// Chat decode tokens per second; missing from older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_tps: Option<f64>,
    /// Tokens per second reading a 4K prompt, if the benchmark said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tps: Option<f64>,
    /// What it was measured with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_projection: Option<bool>,
    /// Short commit of the Strata folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why the run in a comparison gave no result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The speeds a benchmark printed so far.
#[derive(Clone, Copy, Debug, Default)]
struct BenchReading {
    decode: Option<f64>,
    prompt: Option<f64>,
}

impl BenchReading {
    /// From the short table `tools/strata_bench.py` prints at the end.
    fn read(&mut self, line: &str) {
        if let Some(tps) = table_value(line, "chat decode tok/s") {
            self.decode = Some(tps);
        } else if let Some(tps) = table_value(line, "4K prompt tok/s") {
            self.prompt = Some(tps);
        }
    }
}

/// `  chat decode tok/s     38.50   (37.10 - 39.20, n=3)` → 38.5 for
/// `chat decode tok/s`.
fn table_value(line: &str, name: &str) -> Option<f64> {
    line.trim()
        .strip_prefix(name)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Runs Strata's `BENCH.bat` against the running Strata and keeps its
/// speeds for the model.
async fn benchmark(agent: &Arc<Agent>, id: &str) -> Result<Option<String>, Step> {
    let record = measure(agent, id).await?;
    let result = record.result.clone();
    keep_benchmark(agent, record);
    Ok(Some(result))
}

/// One benchmark of the running model `id`.
async fn measure(agent: &Arc<Agent>, id: &str) -> Result<Benchmark, Step> {
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
    agent.jobs.inner.lock().unwrap().bench = BenchReading::default();
    let dir = agent.cfg.strata_dir.clone();
    let install = Installation::of(&agent.cfg);
    let config = install.config_path(id);
    let spec = CommandSpec {
        program: dir.join("BENCH.bat"),
        args: vec!["--config".into(), config.display().to_string()],
        dir: Some(dir.clone()),
    };
    command(agent, spec, false).await?;
    let reading = agent.jobs.inner.lock().unwrap().bench;
    let tps = reading
        .decode
        .ok_or_else(|| "Der Benchmark hat keine Tokens/s gemeldet".to_string())?;
    let model = install.read_model(id);
    Ok(Benchmark {
        model: id.to_string(),
        result: format!("{} Token/s", round1(tps)),
        at: unix_now(),
        decode_tps: Some(round1(tps)),
        prompt_tps: reading.prompt.map(round1),
        context: model.context(),
        speed_projection: Some(model.speed_projection()),
        version: git_commit(&dir),
        error: None,
    })
}

/// Benchmarks each model in turn, each as it is set up, and makes `active`
/// the active model again at the end, also after a cancel. A model that
/// does not come up or report keeps its error and the next one follows.
async fn compare_models(
    agent: &Arc<Agent>,
    ids: &[String],
    active: &str,
) -> Result<Option<String>, Step> {
    let mut outcome = Ok(());
    for (i, id) in ids.iter().enumerate() {
        agent.jobs.inner.lock().unwrap().step_prefix =
            Some(format!("{}/{}: {id}", i + 1, ids.len()));
        let measured = async {
            if active_model(agent).as_deref() != Some(id.as_str()) {
                activate(agent, id).await?;
            }
            measure(agent, id).await
        }
        .await;
        match measured {
            Ok(record) => keep_benchmark(agent, record),
            Err(Step::Cancelled) => {
                outcome = Err(Step::Cancelled);
                break;
            }
            Err(Step::Failed(error)) => {
                tracing::warn!("Vergleich: {id} ohne Ergebnis: {error}");
                let model = Installation::of(&agent.cfg).read_model(id);
                keep_benchmark(
                    agent,
                    Benchmark {
                        model: id.clone(),
                        result: String::new(),
                        at: unix_now(),
                        decode_tps: None,
                        prompt_tps: None,
                        context: model.context(),
                        speed_projection: Some(model.speed_projection()),
                        version: git_commit(&agent.cfg.strata_dir),
                        error: Some(error),
                    },
                );
            }
        }
    }
    agent.jobs.inner.lock().unwrap().step_prefix = None;
    if active_model(agent).as_deref() != Some(active) {
        // Back to the model from before, also after a cancel: the job's
        // own cancel is spent, the switch must not end on it.
        agent.jobs.inner.lock().unwrap().cancelled = false;
        set_step(agent, &format!("{active} wieder aktivieren"));
        activate(agent, active).await?;
    }
    outcome?;
    let benches = load_benchmarks(agent);
    let fastest = benches
        .iter()
        .filter(|b| ids.contains(&b.model))
        .filter_map(|b| Some((b, b.decode_tps?)))
        // The first of equal ones.
        .reduce(|a, b| if b.1 > a.1 { b } else { a });
    Ok(fastest.map(|(b, _)| format!("Am schnellsten: {} mit {}", b.model, b.result)))
}

/// Keeps a model's record in place of its last one; the newest also goes
/// to the file older services read.
fn keep_benchmark(agent: &Agent, mut record: Benchmark) {
    let dir = agent.cfg.state_dir();
    let mut all = load_benchmarks(agent);
    // A failed run keeps the model's last measurement next to its error, so
    // one failure does not wipe a good result.
    if record.error.is_some()
        && let Some(last) = all
            .iter()
            .find(|b| b.model.eq_ignore_ascii_case(&record.model) && b.decode_tps.is_some())
    {
        record = Benchmark {
            error: record.error,
            ..last.clone()
        };
    }
    all.retain(|b| !b.model.eq_ignore_ascii_case(&record.model));
    if record.error.is_none() {
        let text = serde_json::to_string_pretty(&record).expect("benchmark serializes");
        if let Err(e) = crate::write_replacing(&dir.join(BENCH_FILE), &text) {
            tracing::warn!("{BENCH_FILE} nicht gespeichert: {e}");
        }
    }
    all.push(record);
    let text = serde_json::to_string_pretty(&all).expect("benchmarks serialize");
    if let Err(e) = crate::write_replacing(&dir.join(BENCHES_FILE), &text) {
        tracing::warn!("{BENCHES_FILE} nicht gespeichert: {e}");
    }
}

/// The last successful run, of any model.
fn load_benchmark(agent: &Agent) -> Option<Benchmark> {
    let text = std::fs::read_to_string(agent.cfg.state_dir().join(BENCH_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Each model's last run; before the first run of this version, the one
/// run an older service kept.
fn load_benchmarks(agent: &Agent) -> Vec<Benchmark> {
    match std::fs::read_to_string(agent.cfg.state_dir().join(BENCHES_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => load_benchmark(agent)
            .map(|mut b| {
                b.decode_tps = b.decode_tps.or_else(|| {
                    b.result
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok())
                });
                vec![b]
            })
            .unwrap_or_default(),
    }
}

/// Puts the key into every model's config and restarts Strata with it.
async fn set_key(agent: &Arc<Agent>, api_key: String) -> Result<Option<String>, Step> {
    wait_idle(agent).await?;
    set_step(agent, "Key eintragen");
    let install = Installation::of(&agent.cfg);
    let key = api_key.clone();
    tokio::task::spawn_blocking(move || install.set_key(&key))
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

/// Switches the speed projection in a model's config, with the flags setup
/// writes for it; everything else in the config stays. The active model
/// restarts with it once Strata has no request at work.
async fn set_speed_projection(
    agent: &Arc<Agent>,
    id: &str,
    on: bool,
) -> Result<Option<String>, Step> {
    set_step(agent, "Konfiguration ändern");
    change_model(agent, id, move |model| model.set_speed_projection(on)).await?;
    // The engine reads its flags at the start: the active model starts again.
    if active_model(agent).as_deref() == Some(id) {
        wait_idle(agent).await?;
        set_step(agent, "Strata neu starten");
        agent.stop_strata();
        restart_strata(agent).await;
    }
    Ok(None)
}

/// Sets a model's context in its config, without setup; the settings setup
/// derived from the old one stay. The active model restarts with it once
/// Strata has no request at work; if it does not come up, its old config
/// comes back and Strata starts with that.
async fn set_context(agent: &Arc<Agent>, id: &str, context: u32) -> Result<Option<String>, Step> {
    set_step(agent, "Konfiguration ändern");
    let before = change_model(agent, id, move |model| {
        model.set_context(context);
        Ok(())
    })
    .await?;
    if active_model(agent).as_deref() != Some(id) {
        return Ok(None);
    }
    wait_idle(agent).await?;
    set_step(agent, "Strata neu starten");
    agent.stop_strata();
    restart_strata(agent).await;
    let started = if agent.gaming() {
        Ok(true)
    } else {
        started_with_new_args(agent).await
    };
    if let Ok(true) = started {
        return Ok(None);
    }
    let output = agent.output.last_lines(KEPT_LINES);
    set_step(agent, "Alten Kontext zurückholen");
    change_model(agent, id, move |model| {
        model.restore_args(&before);
        Ok(())
    })
    .await?;
    agent.stop_strata();
    restart_strata(agent).await;
    if let Some(job) = agent.jobs.inner.lock().unwrap().current.as_mut() {
        job.lines = output;
    }
    // A cancelled wait also leaves the old context, not an unproven one.
    started?;
    Err(Step::Failed(format!(
        "Strata ist mit {}K Kontext nicht angelaufen; der alte Kontext gilt wieder",
        context / 1024
    )))
}

/// Changes a model's config off the runtime's threads; gives it from before.
async fn change_model(
    agent: &Agent,
    id: &str,
    change: impl FnOnce(&mut Model) -> Result<(), String> + Send + 'static,
) -> Result<Model, Step> {
    let install = Installation::of(&agent.cfg);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let before = install.model(&id)?;
        let mut model = before.clone();
        change(&mut model)?;
        install.save(&model)?;
        Ok(before)
    })
    .await
    .map_err(|e| Step::Failed(e.to_string()))?
    .map_err(Step::Failed)
}

/// Whether the restarted Strata answers: `false` once it has ended or not
/// answered within `context_start_secs`.
async fn started_with_new_args(agent: &Agent) -> Result<bool, Step> {
    set_step(agent, "Warten, bis Strata bereit ist");
    let limit = Duration::from_secs(agent.cfg.context_start_secs);
    let started = Instant::now();
    loop {
        if agent.jobs.inner.lock().unwrap().cancelled {
            return Err(Step::Cancelled);
        }
        if agent.strata_healthy().await {
            return Ok(true);
        }
        if !agent.output.reading() || started.elapsed() >= limit {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// [`Installation::manage`] off the runtime's threads.
async fn manage(agent: &Agent) -> Vec<String> {
    let install = Installation::of(&agent.cfg);
    tokio::task::spawn_blocking(move || install.manage())
        .await
        .unwrap_or_default()
}

/// Whether the active model is among `changed`.
fn active_changed(agent: &Agent, changed: &[String]) -> bool {
    active_model(agent).is_some_and(|a| changed.iter().any(|id| id.eq_ignore_ascii_case(&a)))
}

/// At the service's start, before it takes jobs: brings every model config
/// to what the service keeps in it. Gives what is left to do: a running
/// Strata whose active model's config changed starts again with it, once it
/// has no request at work; a job that runs by then restarts it anyway.
pub async fn manage_configs_at_start(agent: &Arc<Agent>) -> impl Future<Output = ()> + use<> {
    let changed = manage(agent).await;
    let agent = Arc::clone(agent);
    async move {
        if !active_changed(&agent, &changed) || !agent.strata_healthy().await {
            return;
        }
        while agent.strata_busy().await {
            tokio::time::sleep(POLL).await;
        }
        if agent.gaming() || agent.jobs.running() {
            return;
        }
        tracing::info!("Konfiguration des aktiven Modells angepasst, Strata startet neu");
        agent.stop_strata();
        restart_strata(&agent).await;
    }
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
    incoming: Option<Incoming>,
}

/// What an update would bring: the commits Strata's Git source has and this
/// folder lacks, for the Proxy's summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incoming {
    /// Full hash of the upstream commit.
    pub upstream: String,
    /// All incoming commits; `commits` holds at most `MAX_COMMITS` of them.
    pub total: u64,
    /// Newest first.
    pub commits: Vec<IncomingCommit>,
    pub files: Vec<ChangedFile>,
    /// URL of the upstream remote.
    pub remote: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IncomingCommit {
    pub hash: String,
    /// Commit time, Unix seconds.
    pub time: u64,
    pub subject: String,
    pub body: String,
}

/// A file the incoming commits change; binary files count 0 lines.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

/// Most incoming commits the page carries.
const MAX_COMMITS: usize = 200;

impl UpdateCheck {
    pub fn available(&self) -> Option<bool> {
        self.inner.lock().unwrap().available
    }

    pub fn incoming(&self) -> Option<Incoming> {
        self.inner.lock().unwrap().incoming.clone()
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

    fn found(&self, available: Option<bool>, incoming: Option<Incoming>) {
        let mut state = self.inner.lock().unwrap();
        state.asking = false;
        state.checked = Some(Instant::now());
        state.available = available;
        state.incoming = incoming;
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
        let found = incoming_commits(&agent).await;
        let available = found.as_ref().map(|i| i.is_some());
        agent.updates.found(available, found.flatten());
    });
}

/// `None` if Git cannot tell; `Some(None)` when nothing is incoming.
async fn incoming_commits(agent: &Agent) -> Option<Option<Incoming>> {
    let git = find_git(agent).await?;
    let dir = agent.cfg.strata_dir.clone();
    let run = |args: &[&str]| {
        let spec = CommandSpec {
            program: git.clone(),
            args: args.iter().map(|a| a.to_string()).collect(),
            dir: Some(dir.clone()),
        };
        async move {
            let (code, lines) = output(agent, &spec).await?;
            (code == 0).then_some(lines)
        }
    };
    run(&["fetch", "--quiet"]).await?;
    let total: u64 = run(&["rev-list", "--count", "HEAD..@{u}"])
        .await?
        .iter()
        .find_map(|l| l.trim().parse().ok())?;
    if total == 0 {
        return Some(None);
    }
    let upstream = run(&["rev-parse", "@{u}"])
        .await?
        .first()?
        .trim()
        .to_string();
    let max = MAX_COMMITS.to_string();
    let log = run(&[
        "log",
        "-n",
        &max,
        "--format=%H%x1f%ct%x1f%s%x1f%b%x1e",
        "HEAD..@{u}",
    ])
    .await?;
    let numstat = run(&["diff", "--numstat", "HEAD..@{u}"]).await?;
    let remote = run(&["ls-remote", "--get-url"])
        .await
        .and_then(|l| l.first().map(|u| u.trim().to_string()))
        .filter(|u| !u.is_empty());
    Some(Some(Incoming {
        upstream,
        total,
        commits: parse_log(&log.join("\n")),
        files: numstat.iter().filter_map(|l| parse_numstat(l)).collect(),
        remote,
    }))
}

/// Records of `%H %ct %s %b`, fields split by 0x1f, records ended by 0x1e.
fn parse_log(text: &str) -> Vec<IncomingCommit> {
    text.split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').splitn(4, '\u{1f}');
            Some(IncomingCommit {
                hash: fields.next()?.trim().to_string(),
                time: fields.next()?.trim().parse().ok()?,
                subject: fields.next()?.trim().to_string(),
                body: fields.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

/// `added<TAB>removed<TAB>path`; binary files show `-` for both.
fn parse_numstat(line: &str) -> Option<ChangedFile> {
    let mut fields = line.splitn(3, '\t');
    let count = |f: Option<&str>| f.map(|n| n.parse().unwrap_or(0));
    Some(ChangedFile {
        added: count(fields.next())?,
        removed: count(fields.next())?,
        path: fields.next()?.to_string(),
    })
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
    if !Installation::of(&agent.cfg).models().is_empty() {
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
                step_prefix,
                ..
            } = &mut *inner;
            let line = match secret.as_deref() {
                Some(key) => line.replace(key, "***"),
                None => line,
            };
            bench.read(&line);
            let Some(job) = current.as_mut() else {
                continue;
            };
            let before = job.step.clone();
            if progress {
                p.read(&line, job);
                // In a comparison: which model the step belongs to.
                if job.step != before
                    && let (Some(prefix), Some(step)) = (step_prefix.as_deref(), job.step.as_mut())
                {
                    *step = format!("{prefix} · {step}");
                }
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
    let mut inner = agent.jobs.inner.lock().unwrap();
    let step = match &inner.step_prefix {
        Some(prefix) => format!("{prefix} · {step}"),
        None => step.to_string(),
    };
    if let Some(job) = inner.current.as_mut() {
        job.step = Some(step);
        job.percent = None;
    }
    drop(inner);
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
        JobKind::SetContext => "Kontext ändern",
        JobKind::CompareModels => "Modelle vergleichen",
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
    /// What the newer commits bring, while there are some.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<Incoming>,
    pub models: Vec<InstalledModel>,
    pub families: Vec<FamilyView>,
    pub catalog: Vec<CatalogEntry>,
    pub recommended: Option<String>,
    pub contexts: Vec<u32>,
    pub default_context: u32,
    pub ram_gb: f64,
    pub disk_free_gb: Option<f64>,
    pub data_dir: String,
    /// The last successful run, for proxies that know only one.
    pub benchmark: Option<Benchmark>,
    /// Each model's last run.
    pub benchmarks: Vec<Benchmark>,
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
    /// Strata's recovery from repeated reasoning passages, as its config
    /// sets it: `off`, `stop` or `recover`.
    pub loop_recovery: String,
    /// What a request without sampling fields gets; `None` while Strata's
    /// version cannot be read.
    pub sampling: Option<ModelSampling>,
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
    let install = Installation::of(cfg);
    let support = install.support();
    let mut models: Vec<InstalledModel> = install
        .models()
        .into_iter()
        .map(|id| {
            let size = catalog::by_id(&id);
            let id = size.map_or(id, |s| s.id());
            let model = install.read_model(&id);
            InstalledModel {
                disk_bytes: install.disk_bytes(&id),
                active: active.as_deref() == Some(id.as_str()),
                speed_projection: model.speed_projection(),
                context: model.context(),
                loop_recovery: model.loop_recovery().into(),
                sampling: support.map(|support| model.sampling(support)),
                family: size.map(|s| s.family.to_string()),
                size: size.map(|s| s.size.to_string()),
                id,
            }
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    StrataView {
        installed: install.is_set_up(),
        version: git_commit(dir),
        update_available: agent.updates.available(),
        update: agent.updates.incoming(),
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
                partly_downloaded: install.partly_downloaded(&s.id()),
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
        benchmarks: load_benchmarks(agent),
        job: agent.jobs.current(),
    }
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
