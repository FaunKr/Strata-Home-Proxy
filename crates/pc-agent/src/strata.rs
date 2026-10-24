//! Strata management for the web UI: what is installed, and jobs that set up,
//! add, switch, delete, update, benchmark or compare models. At most one job runs at a
//! time; it keeps the PC awake and the watchdog away. Every job runs fixed
//! programs with values from Strata's catalog as the installed Strata reads
//! it ([`crate::catalog`]); requests carry no commands.
//!
//! Each job is a short script: it works through its [`JobContext`] (steps,
//! cancel, waiting, commands, cleanup) and starts and stops Strata only
//! through [`crate::strata_process`].
//!
//! A new setup installs the Strata Terminal ([`crate::terminal`]); a PC
//! with an older Strata folder from source keeps it and its jobs until the
//! switch to the Terminal, which may then delete the old folder.

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    agent::Agent,
    catalog::{self, Catalog, Size},
    command::CommandSpec,
    config::Config,
    install::{Installation, Kind, Model, SWITCH_FILE},
    job::{JobContext, KEPT_LINES, POLL, Step, unix_now},
    managed::Managed,
    releases,
    strata_process::Applied,
    terminal::{self, Terminal},
};
pub use strata_pc_protocol::{
    BenchTool, ChangedFile, Incoming, IncomingCommit, MAX_SWITCHED_CONTEXT, StrataBenchmark,
    StrataFamily, StrataInfo, StrataJob, StrataJobKind, StrataJobRequest, StrataJobState,
    StrataModel, StrataOptions, StrataPresets, StrataReport, StrataSize, StrataTune, TuneMode,
    TuneUse,
};

/// The last benchmark run, as older services kept it alone.
const BENCH_FILE: &str = "strata-benchmark.json";
/// The last benchmark of each model.
const BENCHES_FILE: &str = "strata-benchmarks.json";
/// Each model's latest tune.
const TUNES_FILE: &str = "strata-tunes.json";
/// Where the running tune writes its progress.
const TUNE_STATUS_FILE: &str = "strata-tune-status.json";
/// How often a tune's progress is read.
const TUNE_STATUS_EVERY: Duration = Duration::from_secs(1);
/// What `strata --version` said last, with the build it was for.
const VERSION_FILE: &str = "strata-version.json";
/// There since the Strata Terminal's Strata first answered `/health`.
const SERVED_FILE: &str = "strata-terminal-served";
/// How often the page's visits ask Git whether Strata has a newer version.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(3600);
/// How soon Strata's options are read again after a read that failed or
/// lacked a part.
const OPTIONS_RETRY: Duration = Duration::from_secs(600);
/// How soon Strata's catalog is read again after a read that failed.
const CATALOG_RETRY: Duration = Duration::from_secs(600);
/// How long reading Strata's catalog may take: Strata detects the hardware
/// for it (`nvidia-smi`, the display adapters).
const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);
/// How long reading Strata's options may take; the engine's `--help` alone
/// may take 20 s in Strata's own read.
const OPTIONS_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a model's presets are read again: Strata's verdict follows
/// driver and engine changes.
const PRESETS_EVERY: Duration = Duration::from_secs(1800);
/// How long `strata presets status` may take.
const PRESETS_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the page's visits ask the Strata Terminal for an update.
const TERMINAL_CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How long a benchmark waits for Strata to load its model.
const READY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Strata's port, as the proxy expects it.
const STRATA_PORT: &str = "8080";

/// Where Strata listens: on this PC only in relay mode, else for the NAS too.
pub(crate) fn strata_host(cfg: &Config) -> &'static str {
    if cfg.relay { "127.0.0.1" } else { "0.0.0.0" }
}

/// Why a job was not started: a value outside the catalog (400), or a state
/// that forbids it now (409).
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    BadRequest(String),
    Conflict(String),
}

impl Refusal {
    fn into_text(self) -> String {
        match self {
            Self::BadRequest(why) | Self::Conflict(why) => why,
        }
    }
}

/// A model to install, as a request asks for it.
struct Wanted {
    family: String,
    size: String,
    context: u32,
    speed_projection: bool,
    pictures: bool,
}

/// A model to install, checked against Strata's catalog.
struct NewModel {
    size: Size,
    context: u32,
    speed_projection: bool,
    pictures: bool,
}

/// The model of an install: checked, or checked once the Strata Terminal
/// is there and Strata's catalog can be read.
enum Choice {
    Checked(NewModel),
    Later(Wanted),
}

/// A checked request: what the job runs with.
enum Plan {
    Install {
        choice: Choice,
    },
    AddModel {
        model: NewModel,
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
    SetPictures {
        id: String,
        on: bool,
        /// Setup runs for it again, with its family and size.
        size: Size,
    },
    /// Benchmarks `ids` in turn, then `active` is the active model again.
    CompareModels {
        ids: Vec<String>,
        active: String,
    },
    /// The Strata Terminal serves the active model `id` in place of the old
    /// Strata folder.
    SwitchToTerminal {
        id: String,
        /// The Terminal's setup runs for it, with its family and size.
        size: Size,
    },
    RemoveOldStrataFolder,
    Tune {
        id: String,
        mode: TuneMode,
        usage: TuneUse,
    },
    SetUserArgs {
        id: String,
        user_args: Vec<String>,
    },
    SetEngineSettings {
        id: String,
        user_args: Option<Vec<String>>,
        env: BTreeMap<String, String>,
    },
    /// Changes which preset Strata starts model `id` with.
    Preset {
        id: String,
        change: PresetChange,
    },
}

impl Plan {
    fn model(&self) -> Option<String> {
        match self {
            Self::Install {
                choice: Choice::Checked(model),
            }
            | Self::AddModel { model } => Some(model.size.id.clone()),
            Self::Install {
                choice: Choice::Later(_),
            } => None,
            Self::Activate { id }
            | Self::Delete { id }
            | Self::Benchmark { id }
            | Self::SetSpeedProjection { id, .. }
            | Self::SetContext { id, .. }
            | Self::SetPictures { id, .. }
            | Self::SetUserArgs { id, .. }
            | Self::SetEngineSettings { id, .. }
            | Self::SwitchToTerminal { id, .. }
            | Self::Tune { id, .. }
            | Self::Preset { id, .. } => Some(id.clone()),
            Self::Update
            | Self::SetKey
            | Self::CompareModels { .. }
            | Self::RemoveOldStrataFolder => None,
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
    // Before the checks: gaming mode reads no catalog for them.
    if agent.gaming() {
        return Err(Refusal::Conflict(
            "Gaming mode is on: switch it off first".into(),
        ));
    }
    let plan = check(agent, request)?;
    match &plan {
        Plan::Install {
            choice: Choice::Checked(model),
        }
        | Plan::AddModel { model } => check_model_space(agent, model)?,
        // The picture file and a little room beside it: the model is there.
        Plan::SetPictures { on: true, .. } => check_space(agent, catalog::PICTURES_GB + 1.0)?,
        _ => {}
    }
    let (job, record) = JobContext::begin(agent, kind, plan.model()).map_err(|running| {
        Refusal::Conflict(format!("A job is running already ({})", kind_text(running)))
    })?;
    tracing::info!(kind = ?record.kind, model = ?record.model, "Strata job started");
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let result = run(&job, plan).await;
        job.finish(result);
        // An install or update may have brought another Strata.
        check_catalog_after_job(&agent);
    });
    Ok(record)
}

/// Strata's experimental speed projection, only where setup offers it.
fn check_speed_projection(id: &str, family: Option<&str>, wanted: bool) -> Result<bool, Refusal> {
    if wanted && !family.is_some_and(takes_speed_projection) {
        return Err(Refusal::BadRequest(format!(
            "The experimental speed projection is only for Qwen3.8-Flash-Next and its Coder, not for {id}"
        )));
    }
    Ok(wanted)
}

/// The families setup offers the speed projection for: its vector is made
/// for the original Qwen3.8-Flash-Next, which the Coder shares (`setup.py`
/// hard-codes them, there is no table to read).
pub(crate) fn takes_speed_projection(family: &str) -> bool {
    matches!(family, "qwen" | "coder")
}

/// A model of Strata's catalog with values it takes.
fn new_model(catalog: &Catalog, wanted: Wanted) -> Result<NewModel, Refusal> {
    let size = catalog.find(&wanted.family, &wanted.size).ok_or_else(|| {
        Refusal::BadRequest(format!(
            "{} {} is not in Strata's catalog",
            wanted.family, wanted.size
        ))
    })?;
    Ok(NewModel {
        context: context_of(catalog, wanted.context)?,
        speed_projection: check_speed_projection(
            &size.id,
            Some(&size.family),
            wanted.speed_projection,
        )?,
        pictures: wanted.pictures,
        size: size.clone(),
    })
}

/// A context length Strata's setup offers.
fn context_of(catalog: &Catalog, n: u32) -> Result<u32, Refusal> {
    if catalog.contexts.contains(&n) {
        Ok(n)
    } else {
        Err(Refusal::BadRequest(format!(
            "There is no context length {n}"
        )))
    }
}

/// Room for a new model's download beside what is there of it.
fn check_model_space(agent: &Agent, model: &NewModel) -> Result<(), Refusal> {
    let pictures_gb = if model.pictures {
        catalog::PICTURES_GB
    } else {
        0.0
    };
    check_space(
        agent,
        model_gb(agent, &model.size) + pictures_gb + catalog::DISK_SPARE_GB,
    )
}

fn check(agent: &Agent, request: StrataJobRequest) -> Result<Plan, Refusal> {
    let dir = &agent.cfg.strata_dir;
    let install = Installation::of(&agent.cfg);
    let now = agent.catalog.now(catalog_key(agent).as_deref());
    let catalog = || now.catalog();
    match request {
        StrataJobRequest::Install {
            family,
            size: s,
            context: c,
            // The key comes with the start (see `strata_key`).
            api_key: _,
            experimental_speed_projection: wanted,
            pictures,
        } => {
            let wanted = Wanted {
                family,
                size: s,
                context: c,
                speed_projection: wanted,
                pictures,
            };
            // Without Strata the Terminal comes first; its catalog checks
            // the model then.
            let choice = match now {
                CatalogNow::NoStrata => Choice::Later(wanted),
                _ => Choice::Checked(new_model(catalog()?, wanted)?),
            };
            Ok(Plan::Install { choice })
        }
        StrataJobRequest::AddModel {
            family,
            size: s,
            context: c,
            api_key: _,
            experimental_speed_projection: wanted,
            pictures,
        } => {
            let wanted = Wanted {
                family,
                size: s,
                context: c,
                speed_projection: wanted,
                pictures,
            };
            let model = new_model(catalog()?, wanted)?;
            strata_installed(&install)?;
            Ok(Plan::AddModel { model })
        }
        StrataJobRequest::Activate { model } => {
            let id = installed_model(agent, &install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!("{id} is active already")));
            }
            Ok(Plan::Activate { id })
        }
        StrataJobRequest::Delete { model } => {
            let id = installed_model(agent, &install, &model)?;
            if active_model(agent).as_deref() == Some(id.as_str()) {
                return Err(Refusal::Conflict(format!(
                    "{id} is active and cannot be deleted; activate another model first"
                )));
            }
            Ok(Plan::Delete { id })
        }
        StrataJobRequest::Update => {
            strata_installed(&install)?;
            if install.kind() == Kind::Source && !dir.join(".git").exists() {
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
            let id = installed_model(agent, &install, &model)?;
            let family = now.found(&id).map(|s| s.family.clone());
            check_speed_projection(&id, family.as_deref(), true)?;
            Ok(Plan::SetSpeedProjection { id, on })
        }
        StrataJobRequest::SetContext { model, context: c } => {
            // An installed model goes by its run config: without a catalog
            // any length up to the switched limit is written there.
            let context = match catalog() {
                Ok(catalog) => context_of(catalog, c)?,
                Err(_) if c > 0 => c,
                Err(_) => return Err(Refusal::BadRequest("There is no context length 0".into())),
            };
            if context > MAX_SWITCHED_CONTEXT {
                return Err(Refusal::BadRequest(format!(
                    "Context length {context} needs rope scaling and only works through setup"
                )));
            }
            let id = installed_model(agent, &install, &model)?;
            Ok(Plan::SetContext { id, context })
        }
        StrataJobRequest::SetPictures { model, on } => {
            let id = installed_model(agent, &install, &model)?;
            let size = in_catalog(catalog()?, &id, "setup cannot set it up again")?;
            Ok(Plan::SetPictures { id, on, size })
        }
        StrataJobRequest::SetUserArgs { model, user_args } => {
            let id = installed_model(agent, &install, &model)?;
            let current = install.read_model(&id);
            if !current.keeps_user_args() {
                return Err(Refusal::Conflict(format!(
                    "The Strata of {id} keeps no engine options set by hand; update Strata"
                )));
            }
            if let Some(why) = current.refused_user_args(&user_args) {
                return Err(Refusal::BadRequest(why));
            }
            Ok(Plan::SetUserArgs { id, user_args })
        }
        StrataJobRequest::SetEngineSettings {
            model,
            user_args,
            env,
        } => {
            let id = installed_model(agent, &install, &model)?;
            let current = install.read_model(&id);
            if let Some(user_args) = &user_args {
                if !current.keeps_user_args() {
                    return Err(Refusal::Conflict(format!(
                        "The Strata of {id} keeps no engine options set by hand; update Strata"
                    )));
                }
                if let Some(why) = current.refused_user_args(user_args) {
                    return Err(Refusal::BadRequest(why));
                }
            }
            if let Some(why) = current.refused_env(&env) {
                return Err(Refusal::BadRequest(why));
            }
            Ok(Plan::SetEngineSettings { id, user_args, env })
        }
        StrataJobRequest::CompareModels => {
            strata_installed(&install)?;
            let active = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("No active model known; activate a model first".into())
            })?;
            let mut ids: Vec<String> = install
                .models()
                .into_iter()
                .map(|id| model_id(agent, &install, &id))
                .collect();
            ids.sort();
            Ok(Plan::CompareModels { ids, active })
        }
        StrataJobRequest::SwitchToTerminal => {
            if install.kind() == Kind::Terminal {
                return Err(Refusal::Conflict(
                    "Strata runs on the Strata Terminal already".into(),
                ));
            }
            strata_installed(&install)?;
            let id = active_model(agent).ok_or_else(|| {
                Refusal::Conflict("No active model known; activate a model first".into())
            })?;
            let size = in_catalog(catalog()?, &id, "the Strata Terminal cannot take it over")?;
            Ok(Plan::SwitchToTerminal { id, size })
        }
        StrataJobRequest::RemoveOldStrataFolder => {
            if !Installation::old(&agent.cfg).exists() {
                return Err(Refusal::Conflict("There is no old Strata folder".into()));
            }
            if install.kind() == Kind::Source {
                return Err(Refusal::Conflict(
                    "Strata still runs from the old Strata folder; switch to the Strata Terminal first"
                        .into(),
                ));
            }
            if !terminal_served(agent) {
                return Err(Refusal::Conflict(
                    "The Strata Terminal's Strata has not served yet; the old Strata folder stays until it has"
                        .into(),
                ));
            }
            Ok(Plan::RemoveOldStrataFolder)
        }
        StrataJobRequest::Tune { model, mode, usage } => {
            let id = installed_model(agent, &install, &model)?;
            if let Some(why) = tune_unavailable(&install) {
                return Err(Refusal::Conflict(why));
            }
            Ok(Plan::Tune { id, mode, usage })
        }
        StrataJobRequest::ApplyPreset { model, preset } => {
            preset_plan(agent, &install, &model, PresetChange::Apply(preset))
        }
        StrataJobRequest::UndoPreset { model, preset } => {
            preset_plan(agent, &install, &model, PresetChange::Undo(preset))
        }
        StrataJobRequest::PresetOff { model } => {
            preset_plan(agent, &install, &model, PresetChange::Off)
        }
    }
}

/// A change of which preset Strata starts a model with.
enum PresetChange {
    /// Makes this preset the active one.
    Apply(String),
    /// Back to the one before this active preset.
    Undo(String),
    /// No preset: the standard settings; the preset stays saved.
    Off,
}

/// Changing the preset of `model`: only on the Strata Terminal, which keeps
/// them, and only for a preset id as Strata makes them.
fn preset_plan(
    agent: &Agent,
    install: &Installation,
    model: &str,
    change: PresetChange,
) -> Result<Plan, Refusal> {
    let id = installed_model(agent, install, model)?;
    if install.kind() != Kind::Terminal {
        return Err(Refusal::Conflict(
            "Presets need the Strata Terminal; this Strata is a folder from source".into(),
        ));
    }
    if let PresetChange::Apply(preset) | PresetChange::Undo(preset) = &change {
        let hex = preset.len() == 32 && preset.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            return Err(Refusal::BadRequest(format!("{preset} is not a preset")));
        }
    }
    Ok(Plan::Preset { id, change })
}

/// Why Strata's tuner cannot tune a model, if it cannot: it runs only from
/// the Strata Terminal. Which families it knows is Strata's to say: a
/// refusal is the tune job's error.
fn tune_unavailable(install: &Installation) -> Option<String> {
    (install.kind() != Kind::Terminal)
        .then(|| "Tuning needs the Strata Terminal; this Strata is a folder from source".into())
}

fn strata_installed(install: &Installation) -> Result<(), Refusal> {
    if install.is_set_up() {
        Ok(())
    } else {
        Err(Refusal::Conflict("Strata is not set up yet".into()))
    }
}

/// An installed model's id, as [`model_id`] spells it.
fn installed_model(agent: &Agent, install: &Installation, model: &str) -> Result<String, Refusal> {
    if !Installation::plain_id(model) {
        return Err(Refusal::BadRequest(format!("{model} is no model id")));
    }
    if !install.has(model) {
        return Err(Refusal::Conflict(format!("{model} is not installed")));
    }
    Ok(model_id(agent, install, model))
}

/// A model's id in the letter case it is shown and kept in: its download
/// folder's as setup named it, else the catalog's, else the run config's.
pub(crate) fn model_id(agent: &Agent, install: &Installation, id: &str) -> String {
    install
        .download_name(id)
        .or_else(|| agent.catalog.id_of(id))
        .unwrap_or_else(|| id.to_string())
}

/// The size of an installed model for a job that runs setup for it, which
/// needs the model's family and size.
fn in_catalog(catalog: &Catalog, id: &str, otherwise: &str) -> Result<Size, Refusal> {
    catalog.by_id(id).cloned().ok_or_else(|| {
        Refusal::Conflict(format!("{id} is not in Strata's catalog, so {otherwise}"))
    })
}

/// The model the managed start starts, in the catalog's letter case: the
/// run config `strata serve` serves, or the model's start script. Without
/// a model's own start script (`START-HERE.bat` starts the model used
/// last), the run config Strata touched last, as Strata picks it. Without any
/// start (after the one-line setup) none: the service starts no model, and
/// activating one sets the start.
pub(crate) fn active_model(agent: &Agent) -> Option<String> {
    let managed = agent.managed();
    let (script, args) = managed.start(&agent.cfg)?;
    let install = Installation::of(&agent.cfg);
    let id = install.active_model(script, args)?;
    Some(model_id(agent, &install, &id))
}

/// What is left to download of a model: of one partly downloaded, only the
/// rest counts.
fn model_gb(agent: &Agent, size: &Size) -> f64 {
    let have_gb = Installation::of(&agent.cfg).downloaded_bytes(&size.id) as f64 / 1e9;
    (size.download_gb - have_gb).max(0.0)
}

/// Refuses a job whose downloads need more than the data drive has free.
fn check_space(agent: &Agent, need_gb: f64) -> Result<(), Refusal> {
    let data = &agent.cfg.strata_data_dir;
    let Some(free) = agent.platform().disk_free(data) else {
        return Ok(());
    };
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
        Plan::Install { choice } => install(job, choice).await,
        Plan::AddModel { model } => add_model(job, &model).await,
        Plan::Activate { id } => activate(job, &id).await,
        Plan::Delete { id } => delete(job, &id).await,
        Plan::Update => update(job).await,
        Plan::Benchmark { id } => benchmark(job, &id).await,
        Plan::SetKey => set_key(job).await,
        Plan::SetSpeedProjection { id, on } => set_speed_projection(job, &id, on).await,
        Plan::SetContext { id, context } => set_context(job, &id, context).await,
        Plan::SetPictures { id, on, size } => set_pictures(job, &id, on, &size).await,
        Plan::SetUserArgs { id, user_args } => set_user_args(job, &id, user_args).await,
        Plan::SetEngineSettings { id, user_args, env } => {
            set_engine_settings(job, &id, user_args, env).await
        }
        Plan::CompareModels { ids, active } => compare_models(job, &ids, &active).await,
        Plan::SwitchToTerminal { id, size } => switch_to_terminal(job, &id, &size).await,
        Plan::RemoveOldStrataFolder => remove_old_strata_folder(job).await,
        Plan::Tune { id, mode, usage } => tune(job, &id, mode, usage).await,
        Plan::Preset { id, change } => change_preset(job, &id, change).await,
    }
}

async fn install(job: &JobContext, choice: Choice) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let strata = Installation::of(&agent.cfg);
    if strata.kind() == Kind::Terminal {
        install_terminal(job).await?;
    }
    let model = match choice {
        Choice::Checked(model) => model,
        Choice::Later(wanted) => {
            job.step("Reading Strata's model list");
            let catalog = read_catalog(agent).await.map_err(Step::Failed)?;
            let model = new_model(&catalog, wanted).map_err(|r| Step::Failed(r.into_text()))?;
            check_model_space(agent, &model).map_err(|r| Step::Failed(r.into_text()))?;
            model
        }
    };
    // Setup writes a key only if one is known; the start below makes the
    // one Strata runs with.
    let key = agent.strata_key.key(&agent.cfg).unwrap_or_default();
    setup(job, &strata, &model, &key).await?;
    // A Strata started by hand ends with the old values, the new one starts
    // with the managed ones.
    agent.strata().stop();
    manage(agent).await;
    agent
        .set_managed(managed_start(agent, &strata, &model.size.id))
        .map_err(|e| Step::Failed(e.to_string()))?;
    agent.strata().start_again().await;
    Ok(None)
}

/// The Strata Terminal with Strata's install script unless it is there,
/// then the engine for this PC. Fails with Strata's reason when the feed
/// has no engine for this PC, before any model download.
async fn install_terminal(job: &JobContext) -> Result<(), Step> {
    let agent = job.agent();
    let terminal = Terminal::of(&agent.cfg);
    if !terminal.is_installed() {
        job.step("Installing the Strata Terminal");
        job.command(terminal.install(&agent.cfg.strata_installer), false)
            .await?;
        if !terminal.is_installed() {
            return Err(format!(
                "The install script ended, but the Strata Terminal is not in {}",
                terminal.root().display()
            )
            .into());
        }
    }
    job.step("Installing the engine for this PC");
    ask_terminal_events(job, terminal.install_engine(), terminal::engine_installed).await
}

/// Runs a Terminal command in the job and reads its answer, which says why
/// also when the command fails.
async fn ask_terminal<T>(
    job: &JobContext,
    spec: CommandSpec,
    read: fn(&[String]) -> Result<T, String>,
) -> Result<T, Step> {
    answer(job.command_output(spec).await, read)
}

/// Like [`ask_terminal`] for a command that reports its way as events
/// (`--output jsonl`), shown as the job's steps and progress.
async fn ask_terminal_events<T>(
    job: &JobContext,
    spec: CommandSpec,
    read: fn(&[String]) -> Result<T, String>,
) -> Result<T, Step> {
    answer(job.command_events(spec).await, read)
}

/// A Terminal command's answer read from its output, unless it was
/// cancelled.
fn answer<T>(
    (ran, output): (Result<(), Step>, Vec<String>),
    read: fn(&[String]) -> Result<T, String>,
) -> Result<T, Step> {
    if let Err(Step::Cancelled) = ran {
        return Err(Step::Cancelled);
    }
    read(&output).map_err(Step::Failed)
}

/// What starts model `id` from now on, and what makes up the running
/// Strata: `strata serve` with its run config, or its start script.
fn managed_start(agent: &Agent, install: &Installation, id: &str) -> Managed {
    match install.kind() {
        Kind::Terminal => {
            let terminal = Terminal::of(&agent.cfg);
            Managed {
                strata_start: Some(terminal.program()),
                strata_args: Terminal::serve_args(
                    &install.config_path(id),
                    strata_host(&agent.cfg),
                    STRATA_PORT,
                ),
                strata_processes: Some(terminal.processes()),
            }
        }
        Kind::Source => Managed {
            strata_start: Some(install.run_script(id)),
            strata_args: Vec::new(),
            strata_processes: Some(vec![crate::platform::folder_entry(&agent.cfg.strata_dir)]),
        },
    }
}

async fn add_model(job: &JobContext, model: &NewModel) -> Result<Option<String>, Step> {
    let agent = job.agent();
    // The key Strata runs with: a model with another key would answer the
    // proxy with 401 until the next start.
    let key = agent.strata_key.key(&agent.cfg).unwrap_or_default();
    let install = Installation::of(&agent.cfg);
    setup(job, &install, model, &key).await?;
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
    let Managed {
        strata_start,
        strata_args,
        strata_processes,
    } = managed_start(agent, &Installation::of(&agent.cfg), id);
    let mut managed = agent.managed();
    managed.strata_start = strata_start;
    managed.strata_args = strata_args;
    managed
        .strata_processes
        .get_or_insert_with(|| strata_processes.unwrap_or_default());
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
    // Before the download folder that may hold its letter case is gone.
    let id = model_id(job.agent(), &install, id);
    let model = id.clone();
    tokio::task::spawn_blocking(move || install.remove(&model))
        .await
        .map_err(|e| Step::Failed(e.to_string()))??;
    job.agent().output.forget_load(&id);
    keep_tune(job.agent(), &id, None);
    job.agent().presets.forget(&id);
    Ok(None)
}

/// Like Strata's `UPDATE.bat`: the newest code, then setup's `--update`
/// with Strata ended, so the engine can be replaced. Model files stay.
async fn update(job: &JobContext) -> Result<Option<String>, Step> {
    let agent = job.agent();
    if Installation::of(&agent.cfg).kind() == Kind::Terminal {
        return update_terminal(job).await;
    }
    let dir = agent.cfg.strata_dir.clone();
    let git = git(job).await?;
    job.step("Fetching the new version");
    let pull = CommandSpec {
        program: git,
        args: vec!["pull".into(), "--ff-only".into()],
        dir: Some(dir.clone()),
        ..Default::default()
    };
    job.command(pull, false).await?;
    agent.strata().wait_idle(job).await?;
    agent.strata().stop();
    let spec = CommandSpec {
        program: dir.join("START-HERE.bat"),
        args: vec!["--update".into()],
        dir: Some(dir),
        ..Default::default()
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

/// `strata update` with Strata stopped: the update refuses while an engine
/// serves, and the new engine's test start needs the card.
async fn update_terminal(job: &JobContext) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let terminal = Terminal::of(&agent.cfg);
    agent.strata().wait_idle(job).await?;
    agent.strata().stop();
    job.step("Updating the engine and the Strata Terminal (download and test start)");
    let updated = ask_terminal_events(job, terminal.update(), terminal::updated).await;
    if updated.is_ok() {
        manage(agent).await;
    }
    agent.strata().start_again().await;
    updated?;
    terminal_version(agent, &terminal).await;
    agent.updates.found(Some(false), None);
    Ok(None)
}

/// Asks `strata --version` and keeps the answer with the build it is for,
/// also over a restart: the update summary is kept per version.
async fn terminal_version(agent: &Agent, terminal: &Terminal) {
    let Some(version) = output(agent, &terminal.version())
        .await
        .and_then(|(_, lines)| terminal::version_text(&lines))
    else {
        return;
    };
    let kept = serde_json::json!({"build": terminal.build(), "version": version});
    if let Err(e) =
        crate::write_replacing(&agent.cfg.state_dir().join(VERSION_FILE), &kept.to_string())
    {
        tracing::warn!("{VERSION_FILE} not saved: {e}");
    }
}

/// Strata's version for the page and the benchmarks: the Terminal's
/// version and build as `strata --version` said it for the build that
/// runs, else only the build; or the Strata folder's short commit.
fn strata_version(agent: &Agent, install: &Installation) -> Option<String> {
    match install.kind() {
        Kind::Terminal => {
            let build = Terminal::of(&agent.cfg).build()?;
            std::fs::read_to_string(agent.cfg.state_dir().join(VERSION_FILE))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .filter(|kept| kept["build"] == build.as_str())
                .and_then(|kept| kept["version"].as_str().map(String::from))
                .or_else(|| Some(format!("build {build}")))
        }
        Kind::Source => git_commit(&agent.cfg.strata_dir),
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Runs Strata's benchmark against the running Strata and keeps its speeds
/// for the model.
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
    let install = Installation::of(&agent.cfg);
    let (tool, tps, prompt) = match install.kind() {
        Kind::Terminal => {
            let speeds = ask_terminal(job, strata_bench(agent), terminal::bench_speeds).await?;
            (BenchTool::StrataBench, speeds.decode, speeds.prompt)
        }
        Kind::Source => {
            let dir = agent.cfg.strata_dir.clone();
            let spec = CommandSpec {
                program: dir.join("BENCH.bat"),
                args: vec![
                    "--config".into(),
                    install.config_path(id).display().to_string(),
                ],
                dir: Some(dir),
                ..Default::default()
            };
            job.command(spec, false).await?;
            let reading = job.bench();
            let tps = reading
                .decode
                .ok_or_else(|| "The benchmark reported no tokens/s".to_string())?;
            (BenchTool::BenchBat, tps, reading.prompt)
        }
    };
    let model = install.read_model(id);
    Ok(StrataBenchmark {
        model: id.to_string(),
        result: format!("{} Token/s", round1(tps)),
        at: unix_now(),
        at_text: String::new(),
        decode_tps: Some(round1(tps)),
        prompt_tps: prompt.map(round1),
        context: model.context(),
        speed_projection: Some(model.speed_projection()),
        version: strata_version(agent, &install),
        error: None,
        tool: Some(tool),
    })
}

/// `strata bench run` against the running Strata. Its report goes to a new
/// folder in the service's folder; only the last one stays.
fn strata_bench(agent: &Agent) -> CommandSpec {
    let reports = agent.cfg.state_dir().join("strata-bench");
    let _ = std::fs::remove_dir_all(&reports);
    if let Err(e) = std::fs::create_dir_all(&reports) {
        tracing::warn!("{} not created: {e}", reports.display());
    }
    let key = agent.strata_key.key(&agent.cfg);
    Terminal::of(&agent.cfg).bench(
        &agent.cfg.strata_url,
        key.as_deref(),
        &reports.join(unix_now().to_string()),
    )
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
                        version: strata_version(agent, &Installation::of(&agent.cfg)),
                        error: Some(error),
                        tool: None,
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

/// Strata's auto-tuner for model `id`, with Strata stopped: the tuner starts
/// the engine itself and needs the graphics card. Strata saves a result
/// worth keeping as the model's active preset; the preset that was active
/// before becomes active again, so nothing is applied until the user says.
/// Strata runs again afterwards, also after a cancel.
async fn tune(
    job: &JobContext,
    id: &str,
    mode: TuneMode,
    usage: TuneUse,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let terminal = Terminal::of(&agent.cfg);
    let config = Installation::of(&agent.cfg).config_path(id);
    let status = agent.cfg.state_dir().join(TUNE_STATUS_FILE);
    let _ = std::fs::remove_file(&status);
    agent.strata().wait_idle(job).await?;
    agent.strata().stop();
    job.step("Starting the tuner");
    let started = Instant::now();
    let ran = {
        let run = job.command_answer(terminal.tune(&config, mode, usage, &status));
        tokio::pin!(run);
        let mut last_step = None;
        // Shows the status file; one log line per new step, with the time
        // since the start.
        let mut show = async || {
            let Some(p) = tokio::fs::read_to_string(&status)
                .await
                .ok()
                .and_then(|text| terminal::tune_progress(&text))
            else {
                return;
            };
            let line = (last_step.as_ref() != Some(&p.step))
                .then(|| format!("[{}] {}", since_text(started.elapsed()), p.step));
            job.tune_status(line, &p.step, p.percent, p.eta_secs, p.speeds);
            last_step = Some(p.step);
        };
        let ran = loop {
            tokio::select! {
                ran = &mut run => break ran,
                () = tokio::time::sleep(TUNE_STATUS_EVERY) => show().await,
            }
        };
        // What the tuner wrote just before it ended.
        show().await;
        ran
    };
    let (ran, output) = ran;
    let read = terminal::tune_result(&output, mode, usage, unix_now());
    // Strata activates the preset it saved; undone also after a cancel, so
    // nothing the tuner found is applied.
    let restored = job
        .cleanup(async {
            let restored = match &read {
                Ok(StrataTune {
                    preset: Some(preset),
                    ..
                }) => {
                    job.step("Keeping your settings active");
                    ask_terminal(job, terminal.undo_preset(preset), terminal::preset_answer).await
                }
                _ => Ok(()),
            };
            job.step("Starting Strata again");
            agent.strata().start_again().await;
            restored
        })
        .await;
    let found = match (ran, read) {
        (Err(Step::Cancelled), _) => return Err(Step::Cancelled),
        (_, Ok(found)) => found,
        // Strata's refusal says why; without an answer the command's own
        // reason says more.
        (Err(failed), Err(why)) => {
            return Err(if terminal::answered(&output) {
                Step::Failed(why)
            } else {
                failed
            });
        }
        (Ok(()), Err(why)) => return Err(Step::Failed(why)),
    };
    let text = tune_text(&found);
    keep_tune(agent, id, found);
    restored.map_err(|e| match e {
        Step::Failed(why) => {
            Step::Failed(format!("{text}, but the tuned settings stay active: {why}"))
        }
        other => other,
    })?;
    Ok(Some(text))
}

/// Applies, undoes or switches off a preset with Strata's `strata
/// presets`. Strata puts the active preset over the run config when the
/// model starts, so the active model starts again.
async fn change_preset(
    job: &JobContext,
    id: &str,
    change: PresetChange,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let terminal = Terminal::of(&agent.cfg);
    let spec = match &change {
        PresetChange::Undo(preset) => {
            job.step("Undoing the preset");
            terminal.undo_preset(preset)
        }
        PresetChange::Apply(preset) => {
            job.step("Applying the preset");
            terminal.apply_preset(preset)
        }
        PresetChange::Off => {
            job.step("Turning the preset off");
            terminal.preset_off(&Installation::of(&agent.cfg).config_path(id))
        }
    };
    let changed = ask_terminal(job, spec, terminal::preset_answer).await;
    agent.presets.forget(id);
    changed?;
    if active_model(agent).as_deref() == Some(id) {
        agent.strata().restart_when_idle(job).await?;
    }
    Ok(None)
}

/// A tune's time so far for its log: `7m`, from an hour on `1h 05m`.
fn since_text(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0..60 => format!("{minutes}m"),
        _ => format!("{}h {:02}m", minutes / 60, minutes % 60),
    }
}

/// The tune's result in a sentence: tok/s before and after and the changed
/// options, or why there is nothing to apply.
fn tune_text(tune: &StrataTune) -> String {
    match tune.code.as_str() {
        "faster" => {
            let speeds = match (tune.base_tps, tune.tuned_tps) {
                (Some(base), Some(tuned)) => {
                    format!("{} → {} Token/s", round1(base), round1(tuned))
                }
                _ => "Faster".into(),
            };
            let gain = tune
                .gain_pct
                .map(|g| format!(" (+{}%)", g.round()))
                .unwrap_or_default();
            if tune.changed.is_empty() {
                format!("{speeds}{gain}")
            } else {
                format!("{speeds}{gain}: {}", tune.changed.join(", "))
            }
        }
        "already_fastest" => "Your current settings are already the fastest".into(),
        _ => tune
            .message
            .clone()
            .unwrap_or_else(|| format!("Not tuned ({})", tune.code)),
    }
}

fn load_tunes(agent: &Agent) -> Vec<(String, StrataTune)> {
    std::fs::read_to_string(agent.cfg.state_dir().join(TUNES_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Keeps model `id`'s tune in place of its last one; `None` forgets it.
fn keep_tune(agent: &Agent, id: &str, tune: impl Into<Option<StrataTune>>) {
    let tune = tune.into();
    let mut all = load_tunes(agent);
    let before = all.len();
    all.retain(|(model, _)| !model.eq_ignore_ascii_case(id));
    if tune.is_none() && all.len() == before {
        return;
    }
    all.extend(tune.map(|t| (id.to_string(), t)));
    let text = serde_json::to_string_pretty(&all).expect("tunes serialize");
    if let Err(e) = crate::write_replacing(&agent.cfg.state_dir().join(TUNES_FILE), &text) {
        tracing::warn!("{TUNES_FILE} not saved: {e}");
    }
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

/// From the old Strata folder to the Strata Terminal with the active model
/// `id`: the Terminal and the engine for this PC, then the Terminal's setup
/// on the same Strata data folder with the model's context and speed
/// projection (files already there are not downloaded again) and its Chat
/// settings; once Strata has no request at work, `strata serve` replaces
/// the old start. Until then the old Strata folder stays the service's
/// Strata ([`SWITCH_FILE`]), also across a restart of the service. Until the
/// Terminal's Strata answers, a failure or a cancel takes the Terminal's
/// run config away again and the old start serves again.
async fn switch_to_terminal(
    job: &JobContext,
    id: &str,
    size: &Size,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let cfg = &agent.cfg;
    let old = Installation::of(cfg);
    let terminal = Installation::terminal(&Terminal::of(cfg), &cfg.strata_data_dir);
    let model = old.read_model(id);
    let switch = Switch {
        model: id.to_string(),
        managed: agent.managed(),
    };
    switch.save(agent).map_err(Step::Failed)?;
    let prepared = async {
        install_terminal(job).await?;
        let key = agent.strata_key.key(cfg).unwrap_or_default();
        let new = NewModel {
            size: size.clone(),
            context: model.context().unwrap_or(catalog::DEFAULT_CONTEXT),
            speed_projection: model.speed_projection(),
            pictures: model.pictures(),
        };
        setup(job, &terminal, &new, &key).await?;
        if !terminal.has(id) {
            return Err(Step::Failed(format!(
                "The Strata Terminal's setup ended without a run config for {id}"
            )));
        }
        let (from, to) = (old.clone(), terminal.clone());
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            to.adopt_chat_settings(&from, &id)?;
            to.manage();
            Ok(())
        })
        .await
        .map_err(|e| Step::Failed(e.to_string()))?
        .map_err(Step::Failed)?;
        agent.strata().wait_idle(job).await
    }
    .await;
    if let Err(stopped) = prepared {
        // The old Strata still runs; only the Terminal's run config goes.
        return Err(switch
            .undo(agent, &terminal)
            .map_or_else(Step::Failed, |()| stopped));
    }
    job.step("Starting Strata on the Strata Terminal");
    agent.strata().stop();
    // The start writes its new key into the service's Strata: the Terminal's.
    let started = agent
        .set_managed(managed_start(agent, &terminal, id))
        .map_err(|e| e.to_string())
        .and_then(|()| Switch::end(agent))
        .map_err(Step::Failed);
    let started = match started {
        Ok(()) => {
            agent.strata().start_again().await;
            agent.strata().came_up(job).await
        }
        Err(e) => Err(e),
    };
    if let Ok(true) = started {
        note_served(agent);
        return Ok(None);
    }
    let output = agent.output.last_lines(KEPT_LINES);
    job.cleanup(async {
        job.step("Starting the old Strata again");
        agent.strata().stop();
        let undone = switch.undo(agent, &terminal);
        agent.strata().start_again().await;
        // Strata's own words explain the failure.
        job.show_lines(output);
        undone
    })
    .await
    .map_err(Step::Failed)?;
    started?;
    Err(Step::Failed(
        "Strata did not start on the Strata Terminal; the old Strata folder serves again".into(),
    ))
}

/// A switch to the Strata Terminal under way: the model it takes over and
/// the start from before, kept in [`SWITCH_FILE`].
#[derive(serde::Serialize, serde::Deserialize)]
struct Switch {
    model: String,
    managed: Managed,
}

impl Switch {
    fn path(agent: &Agent) -> PathBuf {
        agent.cfg.state_dir().join(SWITCH_FILE)
    }

    fn save(&self, agent: &Agent) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).expect("a switch serializes");
        crate::write_replacing(&Self::path(agent), &text)
            .map_err(|e| format!("{SWITCH_FILE} not written: {e}"))
    }

    /// The switch has proven itself as far as the service can tell: the
    /// Terminal is the service's Strata from now on.
    fn end(agent: &Agent) -> Result<(), String> {
        match std::fs::remove_file(Self::path(agent)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{SWITCH_FILE} not deleted: {e}"))
            }
            _ => Ok(()),
        }
    }

    /// Back to the old Strata folder: the Terminal's run config goes and
    /// the start from before applies. Until the run config is gone, the
    /// switch stays on record, so the next start of the service tries again.
    fn undo(&self, agent: &Agent, terminal: &Installation) -> Result<(), String> {
        self.save(agent)?;
        let restored = agent
            .set_managed(self.managed.clone())
            .map_err(|e| e.to_string());
        terminal.remove_config(&self.model)?;
        restored?;
        Self::end(agent)
    }
}

/// At the service's start: undoes a switch to the Strata Terminal that the
/// service's end interrupted before the Terminal's Strata started.
fn undo_interrupted_switch(agent: &Agent) {
    let Ok(text) = std::fs::read_to_string(Switch::path(agent)) else {
        return;
    };
    let undone = serde_json::from_str::<Switch>(&text)
        .map_err(|e| format!("{SWITCH_FILE} is invalid: {e}"))
        .and_then(|switch| {
            let cfg = &agent.cfg;
            let terminal = Installation::terminal(&Terminal::of(cfg), &cfg.strata_data_dir);
            switch.undo(agent, &terminal)
        });
    match undone {
        Ok(()) => tracing::info!("An interrupted switch to the Strata Terminal was undone"),
        Err(e) => tracing::error!("Interrupted switch to the Strata Terminal not undone: {e}"),
    }
}

/// Deletes the old Strata folder; the Strata data folder and the Strata
/// Terminal's folders stay, also where they lie inside it.
async fn remove_old_strata_folder(job: &JobContext) -> Result<Option<String>, Step> {
    job.step("Deleting the old Strata folder");
    let cfg = &job.agent().cfg;
    let old = Installation::old(cfg);
    let mut keep = vec![cfg.strata_home.clone(), cfg.strata_components.clone()];
    // The service's own state, should it lie inside.
    keep.extend(Some(cfg.state_dir()).filter(|d| d.starts_with(&cfg.strata_dir)));
    tokio::task::spawn_blocking(move || old.remove_folder(&keep))
        .await
        .map_err(|e| Step::Failed(e.to_string()))??;
    Ok(None)
}

/// Whether the Strata Terminal's Strata has answered `/health` once.
fn terminal_served(agent: &Agent) -> bool {
    agent.cfg.state_dir().join(SERVED_FILE).exists()
}

/// Notes that Strata answered, if it is the Strata Terminal's: the managed
/// start serves a run config of the Terminal.
pub(crate) fn note_served(agent: &Agent) {
    if terminal_served(agent) {
        return;
    }
    let served = agent
        .managed()
        .start(&agent.cfg)
        .is_some_and(|(_, args)| Terminal::served_config(args).is_some());
    if !served || Installation::of(&agent.cfg).kind() != Kind::Terminal {
        return;
    }
    let path = agent.cfg.state_dir().join(SERVED_FILE);
    if let Err(e) = crate::write_replacing(&path, &unix_now().to_string()) {
        tracing::warn!("{SERVED_FILE} not written: {e}");
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
/// derived from the old one stay. The active model restarts with it as
/// [`change_and_restart`] does.
async fn set_context(job: &JobContext, id: &str, context: u32) -> Result<Option<String>, Step> {
    let failed = format!(
        "Strata did not start with {}K context; the old context applies again",
        context / 1024
    );
    change_and_restart(job, id, "Restoring the old context", failed, move |model| {
        model.set_context(context)
    })
    .await
}

/// Turns picture reading for an installed model on or off: setup runs for
/// that model again, with its family, size, context, speed projection and
/// the key Strata runs with, and `--vision yes` or `--vision no`. A model
/// that is not active is left running as it is. If the active model does not
/// come up with the new config, the whole config from before the setup comes
/// back, Strata starts with it and the job fails.
async fn set_pictures(
    job: &JobContext,
    id: &str,
    on: bool,
    size: &Size,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    let install = Installation::of(&agent.cfg);
    job.step(if on {
        "Setting up picture reading"
    } else {
        "Setting up without picture reading"
    });
    // Setup rewrites the config: all of it is kept to bring back, and the
    // active model is known before setup touches any file (a Strata without
    // its own start script starts the config touched last).
    let before = load_model(agent, id).await?;
    let active = active_model(agent).as_deref() == Some(id);
    let touched = install.modified(id);
    // The key Strata runs with: a model with another one would answer the
    // proxy with 401 until the next start.
    let key = agent.strata_key.key(&agent.cfg).unwrap_or_default();
    let model = NewModel {
        size: size.clone(),
        context: before.context().unwrap_or(catalog::DEFAULT_CONTEXT),
        speed_projection: before.speed_projection(),
        pictures: on,
    };
    let ran = setup(job, &install, &model, &key).await;
    if let Err(step) = ran {
        // A failed or cancelled setup leaves no unproven config behind.
        save_model(agent, before).await?;
        install.set_modified(id, touched);
        return Err(step);
    }
    // What the service keeps in every config (loop recovery, sampling),
    // and the place of this config among the ones touched last.
    manage(agent).await;
    install.set_modified(id, touched);
    if !active {
        return Ok(None);
    }
    let failed = format!(
        "Strata did not start {} picture reading; the settings from before apply again",
        if on { "with" } else { "without" }
    );
    let applied = agent
        .strata()
        .restart_or_roll_back(job, "Restoring the settings from before", || async {
            save_model(agent, before.clone()).await?;
            install.set_modified(id, touched);
            Ok(())
        })
        .await?;
    match applied {
        Applied::Kept => Ok(None),
        Applied::RolledBack => Err(Step::Failed(failed)),
    }
}

/// A model's config as it is, off the runtime's threads; an error for one
/// that cannot be read (unlike [`Installation::read_model`]).
async fn load_model(agent: &Agent, id: &str) -> Result<Model, Step> {
    let install = Installation::of(&agent.cfg);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || install.model(&id))
        .await
        .map_err(|e| Step::Failed(e.to_string()))?
        .map_err(Step::Failed)
}

/// A whole model config back as it was, off the runtime's threads.
async fn save_model(agent: &Agent, model: Model) -> Result<(), Step> {
    let install = Installation::of(&agent.cfg);
    tokio::task::spawn_blocking(move || install.save(&model))
        .await
        .map_err(|e| Step::Failed(e.to_string()))?
        .map_err(Step::Failed)
}

/// Replaces a model's engine options set by hand. The active model restarts
/// with them as [`change_and_restart`] does.
async fn set_user_args(
    job: &JobContext,
    id: &str,
    user_args: Vec<String>,
) -> Result<Option<String>, Step> {
    let failed = "Strata did not start with the new engine options; the old ones apply again";
    change_and_restart(
        job,
        id,
        "Restoring the old engine options",
        failed.into(),
        move |model| model.set_user_args(&user_args),
    )
    .await
}

/// Replaces a model's engine environment and, if given, its engine options
/// set by hand. The active model restarts with them once, as
/// [`change_and_restart`] does.
async fn set_engine_settings(
    job: &JobContext,
    id: &str,
    user_args: Option<Vec<String>>,
    env: BTreeMap<String, String>,
) -> Result<Option<String>, Step> {
    let failed = "Strata did not start with the new engine settings; the old ones apply again";
    change_and_restart(
        job,
        id,
        "Restoring the old engine settings",
        failed.into(),
        move |model| {
            if let Some(user_args) = &user_args {
                model.set_user_args(user_args);
            }
            model.set_env(&env);
        },
    )
    .await
}

/// Changes a model's engine args in its config. The active model restarts
/// with them once Strata has no request at work; if it does not come up,
/// its old args come back (step `restoring`), Strata starts with those and
/// the job fails with `failed`.
async fn change_and_restart(
    job: &JobContext,
    id: &str,
    restoring: &'static str,
    failed: String,
    change: impl FnOnce(&mut Model) + Send + 'static,
) -> Result<Option<String>, Step> {
    let agent = job.agent();
    job.step("Changing the configuration");
    let before = change_model(agent, id, move |model| {
        change(model);
        Ok(())
    })
    .await?;
    if active_model(agent).as_deref() != Some(id) {
        return Ok(None);
    }
    let applied = agent
        .strata()
        .restart_or_roll_back(job, restoring, || async move {
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
        Applied::RolledBack => Err(Step::Failed(failed)),
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
/// After an update that stopped Strata, it starts Strata once, with the
/// configs as they are now.
pub async fn manage_configs_at_start(agent: &Arc<Agent>) -> impl Future<Output = ()> + use<> {
    let after_update = crate::update::take_start_after_update(agent);
    undo_interrupted_switch(agent);
    let changed = manage(agent).await;
    let agent = Arc::clone(agent);
    async move {
        if after_update {
            if agent.may_start_strata_itself() {
                tracing::info!("Strata starts after the update");
                agent.strata().start();
            }
            return;
        }
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

/// Whether Strata has an update: for a Strata folder from source, whether
/// its Git source has commits the folder lacks, asked at most once an hour;
/// for the Strata Terminal, what `strata update --check` says, asked at
/// most once a day. Asked in the background when the page is looked at.
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

impl UpdateCheck {
    pub fn available(&self) -> Option<bool> {
        self.inner.lock().unwrap().available
    }

    pub fn incoming(&self) -> Option<Incoming> {
        self.inner.lock().unwrap().incoming.clone()
    }

    /// Claims the next check, if one is due after `every`.
    fn claim(&self, every: Duration) -> bool {
        let mut state = self.inner.lock().unwrap();
        if state.asking || state.checked.is_some_and(|t| t.elapsed() < every) {
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

/// Asks in the background whether an update is available, if it is time
/// to; the page shows the answer at its next visit.
pub fn check_for_update(agent: &Arc<Agent>) {
    let install = Installation::of(&agent.cfg);
    let (askable, every) = match install.kind() {
        Kind::Terminal => (install.is_set_up(), TERMINAL_CHECK_EVERY),
        Kind::Source => (
            agent.cfg.strata_dir.join(".git").exists(),
            UPDATE_CHECK_EVERY,
        ),
    };
    if !askable || agent.jobs.running() || !agent.updates.claim(every) {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let (available, incoming) = match install.kind() {
            Kind::Terminal => terminal_update(&agent).await,
            Kind::Source => {
                let found = incoming_commits(&agent).await;
                (found.as_ref().map(|i| i.is_some()), found.flatten())
            }
        };
        agent.updates.found(available, incoming);
    });
}

/// Strata's options (`StrataInfo.options`): read in the background when the
/// page is looked at, once per Strata Terminal build and engine.
#[derive(Default)]
pub struct OptionsCheck {
    inner: Mutex<OptionsState>,
}

#[derive(Default)]
struct OptionsState {
    asking: bool,
    read: Option<OptionsRead>,
}

struct OptionsRead {
    key: OptionsKey,
    options: Option<StrataOptions>,
    at: Instant,
}

impl OptionsRead {
    /// With the tuner's table, and the engine's help if there is an engine.
    fn is_whole(&self) -> bool {
        self.options.as_ref().is_some_and(|o| {
            !o.tuner.is_empty() && (self.key.engine.is_none() || o.engine_help.is_some())
        })
    }
}

/// What the options are read for: the Terminal's build and the engine
/// the run config starts, as it names it.
#[derive(Clone, PartialEq, Eq)]
struct OptionsKey {
    version: String,
    engine: Option<String>,
}

impl OptionsCheck {
    /// The options read for the Terminal build `version`.
    pub fn current(&self, version: Option<&str>) -> Option<StrataOptions> {
        let state = self.inner.lock().unwrap();
        let read = state.read.as_ref()?;
        (Some(read.key.version.as_str()) == version)
            .then(|| read.options.clone())
            .flatten()
    }

    fn claim(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        !std::mem::replace(&mut state.asking, true)
    }

    fn due(&self, key: &OptionsKey) -> bool {
        let state = self.inner.lock().unwrap();
        state.read.as_ref().is_none_or(|read| {
            read.key != *key || (!read.is_whole() && read.at.elapsed() >= OPTIONS_RETRY)
        })
    }

    fn done(&self, read: Option<(OptionsKey, Option<StrataOptions>)>) {
        let mut state = self.inner.lock().unwrap();
        state.asking = false;
        if let Some((key, options)) = read {
            state.read = Some(OptionsRead {
                key,
                options,
                at: Instant::now(),
            });
        }
    }
}

/// Reads Strata's options in the background when the Terminal's version or
/// the engine changed since the last read; the page shows them at its next
/// visit.
pub fn check_options(agent: &Arc<Agent>) {
    if agent.gaming() || agent.jobs.running() || !agent.options.claim() {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let wanted = {
            let agent = Arc::clone(&agent);
            tokio::task::spawn_blocking(move || options_wanted(&agent))
                .await
                .ok()
                .flatten()
        };
        let read = match wanted {
            Some((key, spec)) if agent.options.due(&key) => {
                let options = output_within(&agent, &spec, OPTIONS_TIMEOUT)
                    .await
                    .and_then(|lines| terminal::read_options(&lines, &key.version));
                Some((key, options))
            }
            _ => None,
        };
        agent.options.done(read);
    });
}

/// Strata's catalog as the installed Strata read it last, for the Strata it
/// was read from ([`catalog_key`]).
#[derive(Default)]
pub struct CatalogCheck {
    inner: Mutex<CatalogState>,
}

#[derive(Default)]
struct CatalogState {
    asking: bool,
    read: Option<CatalogRead>,
}

struct CatalogRead {
    key: String,
    catalog: Result<Catalog, String>,
    at: Instant,
}

/// What is known of Strata's catalog for the Strata on the PC now.
pub enum CatalogNow {
    Read(Catalog),
    Unreadable(String),
    /// Not read yet for this Strata.
    Reading,
    /// There is no Strata to read it from.
    NoStrata,
}

impl CatalogNow {
    /// The catalog, or why a job that needs it is refused.
    fn catalog(&self) -> Result<&Catalog, Refusal> {
        match self {
            Self::Read(catalog) => Ok(catalog),
            Self::Unreadable(why) => Err(Refusal::Conflict(format!(
                "{why}; update Strata to choose a model"
            ))),
            Self::Reading => Err(Refusal::Conflict(
                "Strata's model list is being read; try again in a moment".into(),
            )),
            Self::NoStrata => Err(Refusal::Conflict("Strata is not set up yet".into())),
        }
    }

    /// The catalog's size for model `id`, if the catalog is read and has it.
    fn found(&self, id: &str) -> Option<&Size> {
        self.catalog().ok()?.by_id(id)
    }

    fn unreadable(&self) -> Option<String> {
        match self {
            Self::Unreadable(why) => Some(why.clone()),
            _ => None,
        }
    }
}

impl CatalogCheck {
    pub fn now(&self, key: Option<&str>) -> CatalogNow {
        let Some(key) = key else {
            return CatalogNow::NoStrata;
        };
        let state = self.inner.lock().unwrap();
        match state.read.as_ref().filter(|read| read.key == key) {
            Some(CatalogRead {
                catalog: Ok(catalog),
                ..
            }) => CatalogNow::Read(catalog.clone()),
            Some(CatalogRead {
                catalog: Err(why), ..
            }) => CatalogNow::Unreadable(why.clone()),
            None => CatalogNow::Reading,
        }
    }

    /// Model `id` in the letter case of the catalog read last.
    fn id_of(&self, id: &str) -> Option<String> {
        let state = self.inner.lock().unwrap();
        let catalog = state.read.as_ref()?.catalog.as_ref().ok()?;
        catalog.by_id(id).map(|s| s.id.clone())
    }

    fn claim(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        !std::mem::replace(&mut state.asking, true)
    }

    /// Whether it was read for `key`, with or without success.
    fn has(&self, key: &str) -> bool {
        let state = self.inner.lock().unwrap();
        state.read.as_ref().is_some_and(|read| read.key == key)
    }

    /// Whether to read it for `key`: not read for it yet, or failed at
    /// least `retry` ago.
    fn due(&self, key: &str, retry: Duration) -> bool {
        let state = self.inner.lock().unwrap();
        state.read.as_ref().is_none_or(|read| {
            read.key != key || (read.catalog.is_err() && read.at.elapsed() >= retry)
        })
    }

    fn keep(&self, key: String, catalog: Result<Catalog, String>) {
        self.inner.lock().unwrap().read = Some(CatalogRead {
            key,
            catalog,
            at: Instant::now(),
        });
    }

    fn done(&self) {
        self.inner.lock().unwrap().asking = false;
    }
}

/// What Strata's catalog is read for: the Terminal's build, or the commit
/// of a Strata folder from source; `None` without Strata.
fn catalog_key(agent: &Agent) -> Option<String> {
    let install = Installation::of(&agent.cfg);
    match install.kind() {
        Kind::Terminal => Terminal::of(&agent.cfg)
            .build()
            .map(|build| format!("terminal {build}")),
        Kind::Source => install.exists().then(|| {
            format!(
                "source {}",
                git_commit(&agent.cfg.strata_dir).unwrap_or_default()
            )
        }),
    }
}

/// How Strata's catalog is read: [`catalog::SCRIPT`] with the Python of
/// the Strata files that run.
fn catalog_command(agent: &Agent) -> CommandSpec {
    let cfg = &agent.cfg;
    let install = Installation::of(cfg);
    let data = &cfg.strata_data_dir;
    match install.kind() {
        Kind::Terminal => Terminal::of(cfg).catalog(install.dir(), data),
        Kind::Source => {
            let dir = &cfg.strata_dir;
            // As `START-HERE.bat`: Strata's own Python when the folder has
            // one, else the one setup made.
            let bundled = dir.join("python").join("python.exe");
            let program = if bundled.exists() {
                bundled
            } else {
                dir.join(".venv").join("Scripts").join("python.exe")
            };
            CommandSpec {
                program,
                args: vec![
                    "-c".into(),
                    catalog::SCRIPT.into(),
                    dir.display().to_string(),
                    data.display().to_string(),
                ],
                dir: Some(dir.clone()),
                env: vec![("PYTHONNOUSERSITE".into(), "1".into())],
            }
        }
    }
}

/// Reads Strata's catalog now and keeps it.
async fn read_catalog(agent: &Agent) -> Result<Catalog, String> {
    let wanted = catalog_key(agent).map(|key| (key, catalog_command(agent)));
    let Some((key, spec)) = wanted else {
        return Err("Strata is not set up yet".into());
    };
    let catalog = output_within(agent, &spec, CATALOG_TIMEOUT)
        .await
        .ok_or_else(|| format!("{} did not start", spec.program.display()))
        .and_then(|lines| catalog::read(&lines));
    if let Err(why) = &catalog {
        tracing::warn!("Strata's catalog: {why}");
    }
    agent.catalog.keep(key, catalog.clone());
    catalog
}

/// Reads Strata's catalog in the background unless it is read for the
/// Strata on the PC: at the service's start and after each job. A page
/// visit never waits for it and runs the script only when another Strata
/// is on the PC (one Strata's own app updated) or a failed read is due
/// again.
pub fn check_catalog(agent: &Arc<Agent>) {
    read_catalog_in_background(agent, CATALOG_RETRY);
}

/// After a job: as [`check_catalog`], and a failed read is tried again at
/// once, so an Install or Update that repaired Strata brings its catalog.
fn check_catalog_after_job(agent: &Arc<Agent>) {
    read_catalog_in_background(agent, Duration::ZERO);
}

fn read_catalog_in_background(agent: &Arc<Agent>, retry: Duration) {
    if agent.gaming() || !agent.catalog.claim() {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let key = {
            let agent = Arc::clone(&agent);
            tokio::task::spawn_blocking(move || catalog_key(&agent))
                .await
                .ok()
                .flatten()
        };
        if key.is_some_and(|key| agent.catalog.due(&key, retry)) {
            let _ = read_catalog(&agent).await;
        }
        agent.catalog.done();
    });
}

/// Reads Strata's catalog now unless it was read for the Strata on the PC,
/// as the read at the service's start does; for tests that put Strata on
/// the PC after the service started.
pub async fn read_catalog_if_new(agent: &Agent) {
    if agent.gaming() {
        return;
    }
    if let Some(key) = catalog_key(agent)
        && !agent.catalog.has(&key)
    {
        let _ = read_catalog(agent).await;
    }
}

/// Each model's presets (`StrataModel.presets`), read in the background
/// when the page is looked at: again when the model's run config, its last
/// tune or the Terminal's build changed, after a job changed them, and every
/// [`PRESETS_EVERY`].
#[derive(Default)]
pub struct PresetsCheck {
    inner: Mutex<PresetsState>,
}

#[derive(Default)]
struct PresetsState {
    asking: bool,
    read: HashMap<String, PresetsRead>,
    /// Counts the jobs that changed presets: a read that began before one
    /// is not kept.
    changes: u64,
}

struct PresetsRead {
    key: u64,
    presets: Option<StrataPresets>,
    at: Instant,
}

impl PresetsCheck {
    /// What was read for model `id` with `key`, if it was.
    fn current(&self, id: &str, key: u64) -> Option<StrataPresets> {
        let state = self.inner.lock().unwrap();
        let read = state.read.get(id).filter(|read| read.key == key)?;
        read.presets.clone()
    }

    /// Model `id`'s presets changed: read them at the next visit.
    pub fn forget(&self, id: &str) {
        let mut state = self.inner.lock().unwrap();
        state.read.remove(id);
        state.changes += 1;
    }

    fn changes(&self) -> u64 {
        self.inner.lock().unwrap().changes
    }

    fn claim(&self) -> bool {
        let mut state = self.inner.lock().unwrap();
        !std::mem::replace(&mut state.asking, true)
    }

    fn due(&self, id: &str, key: u64) -> bool {
        let state = self.inner.lock().unwrap();
        state.read.get(id).is_none_or(|read| {
            let every = if read.presets.is_some() {
                PRESETS_EVERY
            } else {
                OPTIONS_RETRY
            };
            read.key != key || read.at.elapsed() >= every
        })
    }

    /// Keeps a read that began at `changes`, unless a job changed presets
    /// since.
    fn keep(&self, id: String, key: u64, presets: Option<StrataPresets>, changes: u64) {
        let mut state = self.inner.lock().unwrap();
        if state.changes != changes {
            return;
        }
        let read = PresetsRead {
            key,
            presets,
            at: Instant::now(),
        };
        state.read.insert(id, read);
    }

    fn done(&self) {
        self.inner.lock().unwrap().asking = false;
    }
}

/// What a model's presets are read for: the Terminal's build, the run
/// config as it is, and the preset of the model's last tune.
fn presets_key(install: &Installation, id: &str, build: &str, tune: Option<&StrataTune>) -> u64 {
    let mut hasher = DefaultHasher::new();
    build.hash(&mut hasher);
    std::fs::read(install.config_path(id))
        .unwrap_or_default()
        .hash(&mut hasher);
    tune.and_then(|t| t.preset.as_deref()).hash(&mut hasher);
    hasher.finish()
}

/// Reads the presets of each model whose read is due, one after the
/// other, from the Strata Terminal; the page shows them at its next visit.
pub fn check_presets(agent: &Arc<Agent>) {
    if agent.gaming() || agent.jobs.running() || !agent.presets.claim() {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let wanted = {
            let agent = Arc::clone(&agent);
            tokio::task::spawn_blocking(move || presets_wanted(&agent))
                .await
                .unwrap_or_default()
        };
        for (id, key, spec, last_tuned) in wanted {
            // A job or gaming mode that began meanwhile has the PC first.
            if agent.gaming() || agent.jobs.running() {
                break;
            }
            if !agent.presets.due(&id, key) {
                continue;
            }
            let changes = agent.presets.changes();
            let presets = output_within(&agent, &spec, PRESETS_TIMEOUT)
                .await
                .and_then(|lines| terminal::read_presets(&lines, last_tuned.as_deref()));
            agent.presets.keep(id, key, presets, changes);
        }
        agent.presets.done();
    });
}

/// Each model with what its presets are read for and how; only from a
/// Terminal.
fn presets_wanted(agent: &Agent) -> Vec<(String, u64, CommandSpec, Option<String>)> {
    let install = Installation::of(&agent.cfg);
    if install.kind() != Kind::Terminal {
        return Vec::new();
    }
    let terminal = Terminal::of(&agent.cfg);
    let Some(build) = terminal.build() else {
        return Vec::new();
    };
    let tunes = load_tunes(agent);
    install
        .models()
        .into_iter()
        .map(|id| {
            let id = model_id(agent, &install, &id);
            let tune = tunes
                .iter()
                .find(|(model, _)| model.eq_ignore_ascii_case(&id))
                .map(|(_, tune)| tune);
            let key = presets_key(&install, &id, &build, tune);
            let spec = terminal.presets_status(&install.config_path(&id));
            let last_tuned = tune.and_then(|t| t.preset.clone());
            (id, key, spec, last_tuned)
        })
        .collect()
}

/// What to read the options for and how, with the engine of the active
/// model's run config or else the first model's; only from a Terminal.
fn options_wanted(agent: &Agent) -> Option<(OptionsKey, CommandSpec)> {
    let install = Installation::of(&agent.cfg);
    if install.kind() != Kind::Terminal {
        return None;
    }
    let terminal = Terminal::of(&agent.cfg);
    let version = terminal.build()?;
    let model = active_model(agent)
        .filter(|id| install.has(id))
        .or_else(|| install.models().into_iter().next());
    let engine = model.as_deref().and_then(|id| {
        let model = install.read_model(id);
        let exe = model.fields().get("exe")?.as_str()?.to_string();
        Some((install.config_path(id), exe))
    });
    let spec = terminal.options(engine.as_ref().map(|(config, _)| config.as_path()));
    let key = OptionsKey {
        version,
        engine: engine.map(|(_, exe)| exe),
    };
    Some((key, spec))
}

/// The Terminal's version, whether `strata update --check` offers
/// something newer and, if so, what the commits since the installed build
/// bring.
async fn terminal_update(agent: &Agent) -> (Option<bool>, Option<Incoming>) {
    let terminal = Terminal::of(&agent.cfg);
    terminal_version(agent, &terminal).await;
    let available = output(agent, &terminal.check_update())
        .await
        .and_then(|(_, lines)| {
            terminal::update_available(&lines)
                .inspect_err(|e| tracing::warn!("Update check: {e}"))
                .ok()
        });
    let incoming = match available {
        Some(true) => release_summary(agent, &terminal).await,
        _ => None,
    };
    (available, incoming)
}

/// The commits and files from the installed build to the latest release,
/// from GitHub; `None` if GitHub does not answer.
async fn release_summary(agent: &Agent, terminal: &Terminal) -> Option<Incoming> {
    let ask = |spec: CommandSpec| async move {
        let (code, lines) = output(agent, &spec).await?;
        (code == 0).then_some(lines)
    };
    let commit = terminal.commit()?;
    let tag = releases::latest_tag(&ask(releases::latest_release()).await?)?;
    releases::incoming(&ask(releases::compare(&commit, &tag)).await?, &tag).flatten()
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
            ..Default::default()
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
    let max = releases::MAX_COMMITS.to_string();
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

/// Runs the setup of `install` without questions, with the model's values
/// and the key; with `pictures`, setup also sets up the picture reader.
///
/// Setup rewrites a model's whole run config. A job that runs it again for
/// an installed model does what [`set_pictures`] does: it reads the active
/// model and the config's modification time before setup (a Strata without
/// its own start script starts the config touched last), calls [`manage`]
/// after it, gives the time back, and writes the config from before back
/// when setup fails or a restart does not come up.
async fn setup(
    job: &JobContext,
    install: &Installation,
    model: &NewModel,
    api_key: &str,
) -> Result<(), Step> {
    let agent = job.agent();
    let size = &model.size;
    let mut args: Vec<String> = vec!["--yes".into(), "--no-start".into(), "--no-browser".into()];
    if !install.models().is_empty() {
        args.push("--setup".into());
    }
    args.extend(
        [
            "--family",
            &size.family,
            "--model",
            &size.size,
            "--context",
            &model.context.to_string(),
            "--vision",
            if model.pictures { "yes" } else { "no" },
            "--host",
            strata_host(&agent.cfg),
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
    if takes_speed_projection(&size.family) {
        // Always explicit: otherwise setup takes it over from an earlier install.
        let value = if model.speed_projection { "on" } else { "off" };
        args.extend(["--experimental-speed-projection".into(), value.into()]);
    }
    let spec = match install.kind() {
        Kind::Terminal => Terminal::of(&agent.cfg).setup(args),
        Kind::Source => {
            let dir = &agent.cfg.strata_dir;
            CommandSpec {
                program: dir.join("START-HERE.bat"),
                args,
                dir: Some(dir.clone()),
                ..Default::default()
            }
        }
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
        ..Default::default()
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
            ..Default::default()
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

/// The output of a command outside a job that ends within `limit`; one
/// that takes longer is ended, with what it printed by then.
async fn output_within(agent: &Agent, spec: &CommandSpec, limit: Duration) -> Option<Vec<String>> {
    let mut run = agent.platform().run(spec).ok()?;
    let cancel = run.canceller();
    let mut lines = Vec::new();
    let read = tokio::time::timeout(limit, async {
        while let Some(line) = run.next_line().await {
            lines.push(line);
        }
    })
    .await;
    if read.is_err() {
        cancel();
        tracing::warn!(
            "{} took longer than {limit:?} and was ended",
            spec.program.display()
        );
    }
    Some(lines)
}

/// The folder as a `strata_processes` entry.
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
        StrataJobKind::SetPictures => "Switch picture reading",
        StrataJobKind::SetUserArgs => "Save engine options",
        StrataJobKind::SetEngineSettings => "Save engine settings",
        StrataJobKind::PresetOff => "Turn the preset off",
        StrataJobKind::CompareModels => "Compare models",
        StrataJobKind::SwitchToTerminal => "Switch to the Strata Terminal",
        StrataJobKind::RemoveOldStrataFolder => "Remove the old Strata folder",
        StrataJobKind::Tune => "Tune",
        StrataJobKind::ApplyPreset => "Apply preset",
        StrataJobKind::UndoPreset => "Undo preset",
    }
}

/// `GET /strata`.
pub fn view(agent: &Agent) -> StrataReport {
    let cfg = &agent.cfg;
    let ram_gb = agent.platform().ram_bytes() as f64 / (1u64 << 30) as f64;
    let vram_gb = agent.platform().vram_bytes() as f64 / (1u64 << 30) as f64;
    let active = active_model(agent);
    let install = Installation::of(cfg);
    let now = agent.catalog.now(catalog_key(agent).as_deref());
    let catalog = now.catalog().ok().cloned().unwrap_or_default();
    let support = install.support();
    let tunes = load_tunes(agent);
    let build = (install.kind() == Kind::Terminal)
        .then(|| Terminal::of(cfg).build())
        .flatten();
    let mut models: Vec<StrataModel> = install
        .models()
        .into_iter()
        .map(|id| {
            let size = now.found(&id);
            let id = model_id(agent, &install, &id);
            let model = install.read_model(&id);
            let no_tune = tune_unavailable(&install);
            let last_tune = tunes
                .iter()
                .find(|(model, _)| model.eq_ignore_ascii_case(&id))
                .map(|(_, tune)| tune.clone());
            StrataModel {
                disk_bytes: install.disk_bytes(&id),
                active: active.as_deref() == Some(id.as_str()),
                speed_projection: Some(model.speed_projection()),
                context: model.context(),
                loop_recovery: Some(model.loop_recovery().into()),
                pictures: model.picture_state(),
                sampling: support.map(|support| model.sampling(support)),
                config: model.shareable_config(),
                user_args: model.shareable_user_args(),
                started_args: Some(model.shareable_started_args()),
                env: Some(model.shareable_env()),
                chat_settings: model.shareable_chat(),
                load_lines: agent.output.load_lines(&id),
                tunable: Some(no_tune.is_none()),
                tune_unavailable: no_tune,
                presets: build.as_deref().and_then(|build| {
                    let key = presets_key(&install, &id, build, last_tune.as_ref());
                    agent.presets.current(&id, key)
                }),
                last_tune,
                family: size.map(|s| s.family.clone()),
                size: size.map(|s| s.size.clone()),
                id,
            }
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    let version = strata_version(agent, &install);
    let options = (install.kind() == Kind::Terminal)
        .then(|| agent.options.current(Terminal::of(cfg).build().as_deref()))
        .flatten();
    let info = StrataInfo {
        installed: install.is_set_up(),
        install: install.is_set_up().then_some(install.kind().into()),
        old_strata_folder: Installation::old(cfg).exists(),
        terminal_served: terminal_served(agent),
        version,
        options,
        update_available: agent.updates.available(),
        models,
        families: catalog
            .families
            .iter()
            .map(|f| StrataFamily {
                id: f.id.clone(),
                title: f.title.clone(),
                speed_projection: takes_speed_projection(&f.id),
                pictures: f.pictures,
            })
            .collect(),
        catalog: catalog
            .sizes
            .iter()
            .map(|s| StrataSize {
                id: s.id.clone(),
                family: s.family.clone(),
                size: s.size.clone(),
                download_gb: s.download_gb,
                ram_gb: s.ram_gb.ceil() as u32,
                memory_with_vram: s.with_vram,
                fits: s.fits,
                partly_downloaded: install.partly_downloaded(&s.id),
                about: s.about.clone(),
                verdict: s.verdict.clone(),
                experimental: s.experimental,
                pictures: Some(s.pictures),
            })
            .collect(),
        catalog_reading: matches!(now, CatalogNow::Reading),
        catalog_unreadable: now.unreadable(),
        recommended: catalog.recommended.clone(),
        contexts: catalog.contexts.clone(),
        default_context: catalog.default_context,
        ram_gb,
        vram_gb: (vram_gb > 0.0).then_some(vram_gb),
        disk_free_gb: agent
            .platform()
            .disk_free(&cfg.strata_data_dir)
            .map(|b| b as f64 / 1e9),
        data_dir: cfg.strata_data_dir.display().to_string(),
        machine: Some(agent.machine()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tunes_time_reads_in_minutes_and_from_an_hour_on_with_hours() {
        assert_eq!(since_text(Duration::from_secs(59)), "0m");
        assert_eq!(since_text(Duration::from_secs(10 * 60 + 5)), "10m");
        assert_eq!(since_text(Duration::from_secs(65 * 60)), "1h 05m");
        assert_eq!(since_text(Duration::from_secs(2 * 3600)), "2h 00m");
    }
}
