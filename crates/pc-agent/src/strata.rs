//! Strata management for the web UI: what is installed, and jobs that set up,
//! add, switch, delete, update or benchmark models. At most one job runs at a
//! time; it keeps the PC awake and the watchdog away. Every job runs fixed
//! programs with values from [`crate::catalog`]; requests carry no commands.

use std::{
    collections::VecDeque,
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
/// Lines of output a job keeps, for the page to show after a failure.
const KEPT_LINES: usize = 20;
/// Strata's port and network access, as the proxy expects them.
const STRATA_PORT: &str = "8080";
const STRATA_HOST: &str = "0.0.0.0";

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
    },
    AddModel {
        family: String,
        size: String,
        context: u32,
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
    },
    AddModel {
        size: &'static Size,
        context: u32,
    },
}

impl Plan {
    fn kind(&self) -> JobKind {
        match self {
            Self::Install { .. } => JobKind::Install,
            Self::AddModel { .. } => JobKind::AddModel,
        }
    }

    fn model(&self) -> Option<String> {
        match self {
            Self::Install { size, .. } | Self::AddModel { size, .. } => Some(size.id()),
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
    let plan = check(request)?;
    if agent.gaming() {
        return Err(Refusal::Conflict(
            "Gaming-Modus ist an: erst Gaming ausschalten".into(),
        ));
    }
    if let Some(size) = match &plan {
        Plan::Install { size, .. } | Plan::AddModel { size, .. } => Some(*size),
    } {
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

fn check(request: JobRequest) -> Result<Plan, Refusal> {
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
        } => Ok(Plan::Install {
            size: size(&family, &s)?,
            context: context(c)?,
            api_key: check_key(api_key)?,
        }),
        JobRequest::AddModel {
            family,
            size: s,
            context: c,
        } => Ok(Plan::AddModel {
            size: size(&family, &s)?,
            context: context(c)?,
        }),
        _ => Err(Refusal::BadRequest(
            "Diese Art Vorgang gibt es noch nicht".into(),
        )),
    }
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

/// Refuses a download that does not fit on the data drive. A model already
/// partly downloaded needs only the spare room.
fn check_space(agent: &Agent, size: &Size) -> Result<(), Refusal> {
    let data = &agent.cfg.strata_data_dir;
    let Some(free) = agent.platform().disk_free(data) else {
        return Ok(());
    };
    let need_gb = if partly_downloaded(data, size) {
        catalog::DISK_SPARE_GB
    } else {
        size.download_gb + catalog::DISK_SPARE_GB
    };
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
        } => install(agent, size, context, api_key).await,
        Plan::AddModel { size, context } => add_model(agent, size, context).await,
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
    setup(agent, size, context, &api_key).await?;
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
    if !agent.gaming() {
        agent.start_strata();
    }
    Ok(None)
}

async fn add_model(agent: &Arc<Agent>, size: &Size, context: u32) -> Result<Option<String>, Step> {
    if !agent.cfg.strata_dir.join("setup.py").is_file() {
        return Err("Strata ist noch nicht eingerichtet".to_string().into());
    }
    let key = agent.managed().strata_api_key.unwrap_or_default();
    setup(agent, size, context, &key).await?;
    Ok(None)
}

/// Runs Strata's setup without questions, with the fixed values and the key.
async fn setup(agent: &Arc<Agent>, size: &Size, context: u32, api_key: &str) -> Result<(), Step> {
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
    }
    if matches!(size.family, "qwen" | "coder") {
        // As Strata's own MCP tool sets it up.
        args.extend(["--experimental-speed-projection".into(), "off".into()]);
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
    if works(agent, "git".into()).await {
        return Ok("git".into());
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
    // The new Git is not on this process's PATH yet.
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
        if works(agent, candidate.clone()).await {
            return Ok(candidate);
        }
    }
    Err(
        "Git wurde installiert, ist aber nicht zu finden; bitte den PC-Dienst neu starten"
            .to_string()
            .into(),
    )
}

/// Whether `program --version` runs and succeeds.
async fn works(agent: &Agent, program: PathBuf) -> bool {
    let spec = CommandSpec {
        program,
        args: vec!["--version".into()],
        dir: None,
    };
    let Ok(mut run) = agent.platform().run(&spec) else {
        return false;
    };
    while run.next_line().await.is_some() {}
    run.wait().await == Some(0)
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
                ..
            } = &mut *inner;
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
    let dir = agent.cfg.state_dir();
    let tmp = dir.join(format!("{JOB_FILE}.tmp"));
    let text = serde_json::to_string_pretty(&job).expect("job serializes");
    if let Err(e) =
        std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, dir.join(JOB_FILE)))
    {
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
            (!id.is_empty()).then(|| id.to_string())
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
    pub models: Vec<InstalledModel>,
    pub families: Vec<FamilyView>,
    pub catalog: Vec<CatalogEntry>,
    pub recommended: Option<String>,
    pub contexts: Vec<u32>,
    pub default_context: u32,
    pub ram_gb: f64,
    pub disk_free_gb: Option<f64>,
    pub data_dir: String,
    pub job: Option<Job>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FamilyView {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub family: Option<String>,
    pub size: Option<String>,
    pub disk_bytes: u64,
    pub active: bool,
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
    let managed = agent.managed();
    let active = managed
        .strata_start
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_lowercase());
    let mut models: Vec<InstalledModel> = installed_models(dir)
        .map(|id| {
            let size = catalog::by_id(&id);
            let id = size.map_or(id, |s| s.id());
            InstalledModel {
                disk_bytes: dir_size(&cfg.strata_data_dir.join("models").join(&id)),
                active: active.as_deref() == Some(run_script(&id).as_str()),
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
        models,
        families: catalog::FAMILIES
            .iter()
            .map(|f| FamilyView {
                id: f.id.into(),
                title: f.title.into(),
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
        job: agent.jobs.current(),
    }
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
