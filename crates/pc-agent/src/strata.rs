//! Strata management for the web UI: what is installed, and jobs that set up,
//! add, switch, delete, update, benchmark or compare models. At most one job runs at a
//! time; it keeps the PC awake and the watchdog away. Every job runs fixed
//! programs with values from [`crate::catalog`]; requests carry no commands.
//!
//! Each job is a short script: it works through its [`JobContext`] (steps,
//! cancel, waiting, commands, cleanup) and starts and stops Strata only
//! through [`crate::strata_process`].

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    agent::Agent,
    catalog::{self, Size},
    command::CommandSpec,
    install::{Installation, Model},
    job::{JobContext, POLL, Step, unix_now},
    managed::Managed,
    strata_process::Applied,
};
pub use strata_pc_protocol::{
    ChangedFile, Incoming, IncomingCommit, MAX_SWITCHED_CONTEXT, StrataBenchmark, StrataFamily,
    StrataInfo, StrataJob, StrataJobKind, StrataJobRequest, StrataJobState, StrataModel,
    StrataReport, StrataSize,
};

/// The last benchmark run, as older services kept it alone.
const BENCH_FILE: &str = "strata-benchmark.json";
/// The last benchmark of each model.
const BENCHES_FILE: &str = "strata-benchmarks.json";
/// How often the page's visits ask Git whether Strata has a newer version.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(3600);
/// How long a benchmark waits for Strata to load its model.
const READY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Strata's port and network access, as the proxy expects them.
const STRATA_PORT: &str = "8080";
const STRATA_HOST: &str = "0.0.0.0";

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
        speed_projection: bool,
    },
    AddModel {
        size: &'static Size,
        context: u32,
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
    /// A restart: every start brings a new key.
    SetKey,
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
    fn model(&self) -> Option<String> {
        match self {
            Self::Install { size, .. } | Self::AddModel { size, .. } => Some(size.id()),
            Self::Activate { id }
            | Self::Delete { id }
            | Self::Benchmark { id }
            | Self::SetSpeedProjection { id, .. }
            | Self::SetContext { id, .. } => Some(id.clone()),
            Self::Update | Self::SetKey | Self::CompareModels { .. } => None,
        }
    }
}

/// Starts a job, or says why not.
pub fn start(agent: &Arc<Agent>, request: StrataJobRequest) -> Result<StrataJob, Refusal> {
    // The service ends after an update, and a job with it.
    if agent.updating.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(Refusal::Conflict("The PC service is updating".into()));
    }
    let kind = request.kind();
    let plan = check(agent, request)?;
    if agent.gaming() {
        return Err(Refusal::Conflict(
            "Gaming mode is on: switch it off first".into(),
        ));
    }
    if let Plan::Install { size, .. } | Plan::AddModel { size, .. } = &plan {
        check_space(agent, size)?;
    }
    let (job, record) = JobContext::begin(agent, kind, plan.model()).map_err(|running| {
        Refusal::Conflict(format!("A job is running already ({})", kind_text(running)))
    })?;
    tracing::info!(kind = ?record.kind, model = ?record.model, "Strata job started");
    tokio::spawn(async move {
        let result = run(&job, plan).await;
        job.finish(result);
    });
    Ok(record)
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
            "The experimental speed projection is only for {}, not for {}",
            offered.join(" and "),
            size.id()
        )));
    }
    Ok(wanted)
}

fn takes_speed_projection(size: &Size) -> bool {
    catalog::family(size.family).is_some_and(|f| f.speed_projection)
}

fn check(agent: &Agent, request: StrataJobRequest) -> Result<Plan, Refusal> {
    let dir = &agent.cfg.strata_dir;
    let install = Installation::of(&agent.cfg);
    let size = |family: &str, size: &str| {
        catalog::find(family, size)
            .ok_or_else(|| Refusal::BadRequest(format!("{family} {size} is not in the catalog")))
    };
    let context = |n: u32| {
        if catalog::CONTEXTS.contains(&n) {
            Ok(n)
        } else {
            Err(Refusal::BadRequest(format!(
                "There is no context length {n}"
            )))
        }
    };
    match request {
        StrataJobRequest::Install {
            family,
            size: s,
            context: c,
            // The key comes with the start (see `strata_key`).
            api_key: _,
            experimental_speed_projection: wanted,
        } => {
            let size = size(&family, &s)?;
            Ok(Plan::Install {
                size,
                context: context(c)?,
                speed_projection: check_speed_projection(size, wanted)?,
            })
        }
        StrataJobRequest::AddModel {
            family,
            size: s,
            context: c,
            api_key: _,
            experimental_speed_projection: wanted,
        } => {
            let size = size(&family, &s)?;
            let plan = Plan::AddModel {
                size,
                context: context(c)?,
                speed_projection: check_speed_projection(size, wanted)?,
            };
            strata_installed(&install)?;
            Ok(plan)
        }
        StrataJobRequest::Activate { model } => {
            let id = installed_model(&install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!("{id} is active already")));
            }
            Ok(Plan::Activate { id })
        }
        StrataJobRequest::Delete { model } => {
            let id = installed_model(&install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!(
                    "{id} is active and cannot be deleted; activate another model first"
                )));
            }
            Ok(Plan::Delete { id })
        }
        StrataJobRequest::Update => {
            strata_installed(&install)?;
            if !dir.join(".git").exists() {
                return Err(Refusal::Conflict(format!(
                    "{} is no Git clone and can only be updated by hand",
                    dir.display()
                )));
            }
            Ok(Plan::Update)
        }
        StrataJobRequest::Benchmark => {
            strata_installed(&install)?;
            let id = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("No active model known; activate a model first".into())
            })?;
            Ok(Plan::Benchmark { id })
        }
        StrataJobRequest::SetKey { api_key: _ } => {
            strata_installed(&install)?;
            Ok(Plan::SetKey)
        }
        StrataJobRequest::SetSpeedProjection { model, on } => {
            let id = installed_model(&install, &model)?;
            let size = catalog::by_id(&id).expect("installed_model checks the catalog");
            check_speed_projection(size, true)?;
            Ok(Plan::SetSpeedProjection { id, on })
        }
        StrataJobRequest::SetContext { model, context: c } => {
            let context = context(c)?;
            if context > MAX_SWITCHED_CONTEXT {
                return Err(Refusal::BadRequest(format!(
                    "Context length {context} needs rope scaling and only works through setup"
                )));
            }
            let id = installed_model(&install, &model)?;
            Ok(Plan::SetContext { id, context })
        }
        StrataJobRequest::CompareModels => {
            strata_installed(&install)?;
            let active = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("No active model known; activate a model first".into())
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
        Err(Refusal::Conflict("Strata is not set up yet".into()))
    }
}

/// The catalog id of an installed model, in the catalog's letter case.
fn installed_model(install: &Installation, model: &str) -> Result<String, Refusal> {
    let id = catalog::by_id(model)
        .ok_or_else(|| Refusal::BadRequest(format!("{model} is not in the catalog")))?
        .id();
    if !install.has(&id) {
        return Err(Refusal::Conflict(format!("{id} is not installed")));
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
            "Not enough space on the drive of {}: {need_gb:.0} GB needed, {free_gb:.0} GB free",
            data.display()
        )));
    }
    Ok(())
}

async fn run(job: &JobContext, plan: Plan) -> Result<Option<String>, Step> {
    match plan {
        Plan::Install {
            size,
            context,
            speed_projection,
        } => install(job, size, context, speed_projection).await,
        Plan::AddModel {
            size,
            context,
            speed_projection,
        } => add_model(job, size, context, speed_projection).await,
        Plan::Activate { id } => activate(job, &id).await,
        Plan::Delete { id } => delete(job, &id).await,
        Plan::Update => update(job).await,
        Plan::Benchmark { id } => benchmark(job, &id).await,
        Plan::SetKey => set_key(job).await,
        Plan::SetSpeedProjection { id, on } => set_speed_projection(job, &id, on).await,
        Plan::SetContext { id, context } => set_context(job, &id, context).await,
        Plan::CompareModels { ids, active } => compare_models(job, &ids, &active).await,
    }
}

async fn install(
    job: &JobContext,
    size: &Size,
    context: u32,
    speed_projection: bool,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let dir = agent.cfg.strata_dir.clone();
    let strata = Installation::of(&agent.cfg);
    if !strata.is_set_up() {
        if dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
            return Err(format!(
                "{} is not empty but no Strata folder; empty it or change strata_dir",
                dir.display()
            )
            .into());
        }
        let git = git(job).await?;
        job.step("Fetching Strata");
        let clone = CommandSpec {
            program: git,
            args: vec![
                "clone".into(),
                agent.cfg.strata_repo.clone(),
                dir.display().to_string(),
            ],
            dir: None,
        };
        job.command(clone, false).await?;
    }
    // Setup writes a key only if one is known; the start below makes the
    // one Strata runs with.
    let key = agent.strata_key.key(&agent.cfg).unwrap_or_default();
    setup(job, size, context, &key, speed_projection).await?;
    // A Strata started by hand ends with the old values, the new one starts
    // with the managed ones.
    agent.strata().stop();
    manage(agent).await;
    let managed = Managed {
        strata_start: Some(strata.run_script(&size.id())),
        strata_args: Vec::new(),
        strata_processes: Some(vec![folder_entry(&dir)]),
    };
    agent
        .set_managed(managed)
        .map_err(|e| Step::Failed(e.to_string()))?;
    agent.strata().start_again().await;
    Ok(None)
}

async fn add_model(
    job: &JobContext,
    size: &Size,
    context: u32,
    speed_projection: bool,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    // The key Strata runs with: a model with another key would answer the
    // proxy with 401 until the next start.
    let key = agent.strata_key.key(&agent.cfg).unwrap_or_default();
    setup(job, size, context, &key, speed_projection).await?;
    if active_changed(agent, &manage(agent).await) {
        agent.strata().restart_when_idle(job).await?;
    }
    Ok(None)
}

async fn activate(job: &JobContext, id: &str) -> Result<Option<String>, Step> {
    let agent = job.agent();
    agent.strata().wait_idle(job).await?;
    job.step("Starting Strata with the new model");
    agent.strata().stop();
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
    agent.strata().start_again().await;
    saved?;
    Ok(None)
}

async fn delete(job: &JobContext, id: &str) -> Result<Option<String>, Step> {
    job.step("Deleting files");
    let install = Installation::of(&job.agent().cfg);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || install.remove(&id))
        .await
        .map_err(|e| Step::Failed(e.to_string()))??;
    Ok(None)
}

/// Like Strata's `UPDATE.bat`: the newest code, then setup's `--update`
/// with Strata ended, so the engine can be replaced. Model files stay.
async fn update(job: &JobContext) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let dir = agent.cfg.strata_dir.clone();
    let git = git(job).await?;
    job.step("Fetching the new version");
    let pull = CommandSpec {
        program: git,
        args: vec!["pull".into(), "--ff-only".into()],
        dir: Some(dir.clone()),
    };
    job.command(pull, false).await?;
    agent.strata().wait_idle(job).await?;
    agent.strata().stop();
    let spec = CommandSpec {
        program: dir.join("START-HERE.bat"),
        args: vec!["--update".into()],
        dir: Some(dir),
    };
    let updated = job.command(spec, true).await;
    if updated.is_ok() {
        manage(agent).await;
    }
    agent.strata().start_again().await;
    updated?;
    agent.updates.found(Some(false), None);
    Ok(None)
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Runs Strata's `BENCH.bat` against the running Strata and keeps its
/// speeds for the model.
async fn benchmark(job: &JobContext, id: &str) -> Result<Option<String>, Step> {
    let record = measure(job, id).await?;
    let result = record.result.clone();
    keep_benchmark(job.agent(), record);
    Ok(Some(result))
}

/// One benchmark of the running model `id`.
async fn measure(job: &JobContext, id: &str) -> Result<StrataBenchmark, Step> {
    let agent = job.agent();
    // A Strata that is still loading gets no second start; one that hangs
    // ends first.
    if !agent.strata_healthy().await && !agent.strata().loading() {
        agent.strata().restart().await;
    }
    job.wait_for("Waiting until Strata is ready", Some(READY_TIMEOUT), || {
        agent.strata_healthy()
    })
    .await?;
    job.step("Running the benchmark (about 10 min)");
    let dir = agent.cfg.strata_dir.clone();
    let install = Installation::of(&agent.cfg);
    let config = install.config_path(id);
    let spec = CommandSpec {
        program: dir.join("BENCH.bat"),
        args: vec!["--config".into(), config.display().to_string()],
        dir: Some(dir.clone()),
    };
    job.command(spec, false).await?;
    let reading = job.bench();
    let tps = reading
        .decode
        .ok_or_else(|| "The benchmark reported no tokens/s".to_string())?;
    let model = install.read_model(id);
    Ok(StrataBenchmark {
        model: id.to_string(),
        result: format!("{} Token/s", round1(tps)),
        at: unix_now(),
        at_text: String::new(),
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
    job: &JobContext,
    ids: &[String],
    active: &str,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let mut outcome = Ok(());
    for (i, id) in ids.iter().enumerate() {
        job.set_prefix(Some(format!("{}/{}: {id}", i + 1, ids.len())));
        let measured = async {
            if active_model(agent).as_deref() != Some(id.as_str()) {
                activate(job, id).await?;
            }
            measure(job, id).await
        }
        .await;
        match measured {
            Ok(record) => keep_benchmark(agent, record),
            Err(Step::Cancelled) => {
                outcome = Err(Step::Cancelled);
                break;
            }
            Err(Step::Failed(error)) => {
                tracing::warn!("Comparison: {id} without result: {error}");
                let model = Installation::of(&agent.cfg).read_model(id);
                keep_benchmark(
                    agent,
                    StrataBenchmark {
                        model: id.clone(),
                        result: String::new(),
                        at: unix_now(),
                        at_text: String::new(),
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
    job.set_prefix(None);
    if active_model(agent).as_deref() != Some(active) {
        // Back to the model from before, also after a cancel.
        job.cleanup(async {
            job.step(&format!("Activating {active} again"));
            activate(job, active).await
        })
        .await?;
    }
    outcome?;
    let benches = load_benchmarks(agent);
    let fastest = benches
        .iter()
        .filter(|b| ids.contains(&b.model))
        .filter_map(|b| Some((b, b.decode_tps?)))
        // The first of equal ones.
        .reduce(|a, b| if b.1 > a.1 { b } else { a });
    Ok(fastest.map(|(b, _)| format!("Fastest: {} with {}", b.model, b.result)))
}

/// Keeps a model's record in place of its last one; the newest also goes
/// to the file older services read.
fn keep_benchmark(agent: &Agent, mut record: StrataBenchmark) {
    let dir = agent.cfg.state_dir();
    let mut all = load_benchmarks(agent);
    // A failed run keeps the model's last measurement next to its error, so
    // one failure does not wipe a good result.
    if record.error.is_some()
        && let Some(last) = all
            .iter()
            .find(|b| b.model.eq_ignore_ascii_case(&record.model) && b.decode_tps.is_some())
    {
        record = StrataBenchmark {
            error: record.error,
            ..last.clone()
        };
    }
    all.retain(|b| !b.model.eq_ignore_ascii_case(&record.model));
    if record.error.is_none() {
        let text = serde_json::to_string_pretty(&record).expect("benchmark serializes");
        if let Err(e) = crate::write_replacing(&dir.join(BENCH_FILE), &text) {
            tracing::warn!("{BENCH_FILE} not saved: {e}");
        }
    }
    all.push(record);
    let text = serde_json::to_string_pretty(&all).expect("benchmarks serialize");
    if let Err(e) = crate::write_replacing(&dir.join(BENCHES_FILE), &text) {
        tracing::warn!("{BENCHES_FILE} not saved: {e}");
    }
}

/// The last successful run, of any model.
fn load_benchmark(agent: &Agent) -> Option<StrataBenchmark> {
    let text = std::fs::read_to_string(agent.cfg.state_dir().join(BENCH_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Each model's last run; before the first run of this version, the one
/// run an older service kept.
fn load_benchmarks(agent: &Agent) -> Vec<StrataBenchmark> {
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

/// Restarts Strata, which brings a new key into every model's config; the
/// key an older proxy sends is not used.
async fn set_key(job: &JobContext) -> Result<Option<String>, Step> {
    let agent = job.agent();
    agent.strata().wait_idle(job).await?;
    job.step("Starting Strata with a new key");
    agent.strata().restart_for(job).await;
    Ok(None)
}

/// Switches the speed projection in a model's config, with the flags setup
/// writes for it; everything else in the config stays. The active model
/// restarts with it once Strata has no request at work.
async fn set_speed_projection(
    job: &JobContext,
    id: &str,
    on: bool,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    job.step("Changing the configuration");
    change_model(agent, id, move |model| model.set_speed_projection(on)).await?;
    // The engine reads its flags at the start: the active model starts again.
    if active_model(agent).as_deref() == Some(id) {
        agent.strata().restart_when_idle(job).await?;
    }
    Ok(None)
}

/// Sets a model's context in its config, without setup; the settings setup
/// derived from the old one stay. The active model restarts with it once
/// Strata has no request at work; if it does not come up, its old config
/// comes back and Strata starts with that.
async fn set_context(job: &JobContext, id: &str, context: u32) -> Result<Option<String>, Step> {
    let agent = job.agent();
    job.step("Changing the configuration");
    let before = change_model(agent, id, move |model| {
        model.set_context(context);
        Ok(())
    })
    .await?;
    if active_model(agent).as_deref() != Some(id) {
        return Ok(None);
    }
    let applied = agent
        .strata()
        .restart_or_roll_back(job, "Restoring the old context", || async move {
            change_model(agent, id, move |model| {
                model.restore_args(&before);
                Ok(())
            })
            .await
            .map(drop)
        })
        .await?;
    match applied {
        Applied::Kept => Ok(None),
        Applied::RolledBack => Err(Step::Failed(format!(
            "Strata did not start with {}K context; the old context applies again",
            context / 1024
        ))),
    }
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
        tracing::info!("Config of the active model changed, Strata restarts");
        agent.strata().restart_unless_job_runs().await;
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
    job: &JobContext,
    size: &Size,
    context: u32,
    api_key: &str,
    speed_projection: bool,
) -> Result<(), Step> {
    let agent = job.agent();
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
        job.hide(api_key);
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
    job.command(spec, true).await
}

/// Git, installed with winget for this user if it is missing.
async fn git(job: &JobContext) -> Result<PathBuf, Step> {
    let agent = job.agent();
    if let Some(git) = find_git(agent).await {
        return Ok(git);
    }
    job.step("Installing Git");
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
    job.command(winget, false).await.map_err(|e| match e {
        Step::Failed(e) => Step::Failed(format!(
            "Git is missing and could not be installed ({e}); please install Git for Windows"
        )),
        cancelled => cancelled,
    })?;
    find_git(agent).await.ok_or_else(|| {
        "Git was installed but cannot be found; please restart the PC service"
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

/// The folder as a `strata_processes` entry.
fn folder_entry(dir: &Path) -> String {
    let mut entry = dir.display().to_string();
    if !crate::platform::is_folder(&entry) {
        entry.push(std::path::MAIN_SEPARATOR);
    }
    entry
}

fn kind_text(kind: StrataJobKind) -> &'static str {
    match kind {
        StrataJobKind::Install => "Setup",
        StrataJobKind::AddModel => "Add model",
        StrataJobKind::Activate => "Activate model",
        StrataJobKind::Delete => "Delete model",
        StrataJobKind::Update => "Update",
        StrataJobKind::Benchmark => "Benchmark",
        StrataJobKind::SetKey => "Set Strata key",
        StrataJobKind::SetSpeedProjection => "Switch speed projection",
        StrataJobKind::SetContext => "Change context",
        StrataJobKind::CompareModels => "Compare models",
    }
}

/// `GET /strata`.
pub fn view(agent: &Agent) -> StrataReport {
    let cfg = &agent.cfg;
    let dir = &cfg.strata_dir;
    let ram_gb = agent.platform().ram_bytes() as f64 / (1u64 << 30) as f64;
    let active = active_model(agent);
    let install = Installation::of(cfg);
    let support = install.support();
    let mut models: Vec<StrataModel> = install
        .models()
        .into_iter()
        .map(|id| {
            let size = catalog::by_id(&id);
            let id = size.map_or(id, |s| s.id());
            let model = install.read_model(&id);
            StrataModel {
                disk_bytes: install.disk_bytes(&id),
                active: active.as_deref() == Some(id.as_str()),
                speed_projection: Some(model.speed_projection()),
                context: model.context(),
                loop_recovery: Some(model.loop_recovery().into()),
                sampling: support.map(|support| model.sampling(support)),
                family: size.map(|s| s.family.to_string()),
                size: size.map(|s| s.size.to_string()),
                id,
            }
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    let info = StrataInfo {
        installed: install.is_set_up(),
        version: git_commit(dir),
        update_available: agent.updates.available(),
        models,
        families: catalog::FAMILIES
            .iter()
            .map(|f| StrataFamily {
                id: f.id.into(),
                title: f.title.into(),
                speed_projection: f.speed_projection,
            })
            .collect(),
        catalog: catalog::SIZES
            .iter()
            .map(|s| StrataSize {
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
    };
    StrataReport {
        info,
        update: agent.updates.incoming(),
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
