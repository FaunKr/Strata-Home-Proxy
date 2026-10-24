//! Strata on the PC through its service: the Strata page and jobs started
//! from it. The Strata key comes from the PC service (`strata_key`) and never
//! reaches the page.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use strata_admin_ui::{
    api::{RelayView, StrataJobForm, StrataPage},
    labels::{NO_AGENT, NO_MAC, NOT_CONFIGURED, strata_job_label},
};

use strata_pc_protocol::{
    PcStatus, StrataInfo, StrataInstall, StrataJob, StrataJobKind, StrataJobRequest,
    StrataJobState, is_env_name,
};

use crate::{
    agent::AgentClient,
    auth::{hex, random_key},
    clock,
    events::EventKind,
    files::write_private,
    state::{AppState, WakeError},
    update_summary::{self, UpdateSummaries},
};

/// Where an older proxy kept a key it handed to the PC; removed at start.
const PENDING_FILE: &str = "strata-pending-key.json";
/// The last answer of the PC service, shown while the PC sleeps; kept over a
/// restart of the proxy.
const LAST_FILE: &str = "strata-last.json";
pub(crate) const BY_UI: &str = "for Strata, from the web UI";
const ASLEEP: &str = "The PC is asleep or its service does not answer. A button here wakes it.";
const TOO_OLD: &str = "The PC service cannot manage Strata yet. Please update the PC service.";
const RELAY_ASLEEP: &str = "The PC is asleep: wake it on the Overview to switch relay mode.";
const RELAY_TOO_OLD: &str = "The PC service is too old for relay mode: update it in the PC area.";
const RELAY_GAMING: &str = "Gaming mode is on: switch it off to switch relay mode.";
const RELAY_OLD_FOLDER: &str = "Relay mode needs the Strata Terminal: Strata still runs from the old Strata folder. Switch to the Terminal under Models first.";

pub struct StrataState {
    /// The last answer of the PC service, shown while the PC sleeps.
    last: Mutex<Option<LastAnswer>>,
    /// A job was started or reported running, and its end is not noted yet.
    job_running: AtomicBool,
    /// What an available update brings.
    pub update: UpdateSummaries,
    dir: Option<PathBuf>,
}

/// What the PC service reported about Strata, without its job: the job may
/// have ended unseen by the time the page shows it.
#[derive(Clone, Serialize, Deserialize)]
struct LastAnswer {
    info: StrataInfo,
    /// When it answered, in Unix seconds.
    at: u64,
}

impl StrataState {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        if let Some(dir) = &data_dir {
            match std::fs::remove_file(dir.join(PENDING_FILE)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    tracing::warn!("{PENDING_FILE} not removed: {e}");
                }
                _ => {}
            }
        }
        let last = data_dir
            .as_ref()
            .and_then(|dir| read_last(&dir.join(LAST_FILE)));
        Self {
            last: Mutex::new(last),
            job_running: AtomicBool::new(false),
            update: UpdateSummaries::new(data_dir.clone()),
            dir: data_dir,
        }
    }

    fn set_last(&self, mut info: StrataInfo) {
        info.job = None;
        let last = LastAnswer {
            info,
            at: clock::unix_now(),
        };
        // Written under the lock: an older answer never overwrites a newer one.
        let mut kept = self.last.lock().unwrap();
        if let Some(dir) = &self.dir
            && let Err(e) = write_private(
                dir,
                LAST_FILE,
                &serde_json::to_vec(&last).expect("last answer serializes"),
            )
        {
            tracing::error!("{LAST_FILE} not saved: {e}");
        }
        *kept = Some(last);
    }
}

/// The kept answer; `None` if there is none or it does not read.
fn read_last(path: &std::path::Path) -> Option<LastAnswer> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("{LAST_FILE} not readable: {e}");
            return None;
        }
    };
    serde_json::from_slice(&bytes)
        .inspect_err(|e| tracing::warn!("{LAST_FILE} not readable: {e}"))
        .ok()
}

impl StrataState {
    /// Whether a Strata job the proxy started or saw is still running.
    pub fn job_running(&self) -> bool {
        self.job_running.load(Ordering::SeqCst)
    }

    /// A run over all models activates each model itself, so it sets the guard
    /// here; a Strata job started by hand stays refused meanwhile.
    pub fn set_job_running(&self, on: bool) {
        self.job_running.store(on, Ordering::SeqCst);
    }

    /// What the PC service reported last, kept over a restart; `None` until
    /// it answered once.
    pub fn last_info(&self) -> Option<StrataInfo> {
        self.last.lock().unwrap().as_ref().map(|l| l.info.clone())
    }

    /// When the kept answer came, in Unix seconds.
    pub fn last_at(&self) -> Option<u64> {
        self.last.lock().unwrap().as_ref().map(|l| l.at)
    }

    /// The last answer with its benchmark times, and the local time it came.
    fn shown_last(&self) -> Option<(StrataInfo, String)> {
        let last = self.last.lock().unwrap().clone()?;
        let mut info = last.info;
        with_local_times(&mut info);
        Some((info, clock::local_text(last.at)))
    }
}

/// The Strata page; asks the PC service but never wakes the PC.
pub async fn page(state: &Arc<AppState>) -> StrataPage {
    let agent = state.agent();
    if !agent.configured() {
        return StrataPage {
            problem: Some(NO_AGENT.into()),
            gaming: false,
            info: None,
            stale: false,
            as_of: None,
            update: None,
            key_unreported: false,
            relay: RelayView {
                blocked: Some(NO_AGENT.into()),
                ..RelayView::default()
            },
        };
    }
    let Some(status) = state.ask_pc(&agent).await else {
        let (info, as_of) = state.strata.shown_last().unzip();
        return StrataPage {
            problem: Some(ASLEEP.into()),
            gaming: false,
            stale: info.is_some(),
            as_of,
            info,
            update: state.strata.update.stale(),
            key_unreported: false,
            relay: RelayView {
                on: state.relay(),
                switching: false,
                blocked: Some(RELAY_ASLEEP.into()),
            },
        };
    };
    let Some((mut info, incoming)) = agent.strata().await else {
        return StrataPage {
            problem: Some(TOO_OLD.into()),
            gaming: status.gaming_mode,
            info: None,
            stale: false,
            as_of: None,
            update: None,
            key_unreported: false,
            relay: relay_view(&status, None),
        };
    };
    with_local_times(&mut info);
    state.strata.set_last(info.clone());
    let job = status.strata_job
        || info
            .job
            .as_ref()
            .is_some_and(|j| j.state == StrataJobState::Running);
    let update = update_summary::noted(
        state,
        update_summary::Now {
            installed: info.version.as_deref(),
            update_available: info.update_available,
            gaming: status.gaming_mode,
            job,
            code_bench: state.code_bench.busy(),
        },
        incoming,
    );
    StrataPage {
        problem: None,
        gaming: status.gaming_mode,
        relay: relay_view(&status, Some(&info)),
        info: Some(info),
        stale: false,
        as_of: None,
        update,
        key_unreported: state.strata_key.unreported(),
    }
}

/// Relay mode as an answering PC service reports it, and why its switch
/// cannot be used now. The switch shows the mode it is set to; until
/// Strata runs with it, it is switching.
fn relay_view(status: &PcStatus, info: Option<&StrataInfo>) -> RelayView {
    let job = info.and_then(|i| i.job.as_ref());
    let running = job
        .is_some_and(|j| j.kind == StrataJobKind::SetRelay && j.state == StrataJobState::Running);
    let wanted = status.relay.and_then(|r| r.wanted);
    let on = wanted.unwrap_or(status.relay_on());
    let switching = running || on != status.relay_on();
    // A service that reports no wanted mode takes no `set_relay`.
    let blocked = if wanted.is_none() {
        Some(RELAY_TOO_OLD)
    } else if status.gaming_mode {
        Some(RELAY_GAMING)
    } else if info.and_then(|i| i.install) == Some(StrataInstall::Source) {
        Some(RELAY_OLD_FOLDER)
    } else {
        None
    };
    RelayView {
        on,
        switching,
        blocked: blocked.map(String::from),
    }
}

/// Turns relay mode on or off with a job on the PC, which keeps the mode
/// and restarts Strata once it is idle; never wakes the PC. The PC
/// service's own refusals (an update, a job running) come back as they are.
pub async fn set_relay(state: &AppState, on: bool) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    let Some(status) = state.ask_pc(&agent).await else {
        return Err(RELAY_ASLEEP.into());
    };
    if let Some(why) = relay_view(&status, None).blocked {
        return Err(why);
    }
    // Set so already, e.g. a second click: no second job or event.
    if status.relay.and_then(|r| r.wanted) == Some(on) {
        return Ok(());
    }
    agent
        .start_strata_job(&StrataJobRequest::SetRelay { on })
        .await?;
    state.strata.job_running.store(true, Ordering::SeqCst);
    state.events.record(
        if on {
            EventKind::RelayOn
        } else {
            EventKind::RelayOff
        },
        None,
    );
    state.overview.refresh_now();
    Ok(())
}

/// Benchmark times in local time, relative to today.
fn with_local_times(info: &mut StrataInfo) {
    for bench in info.benchmark.iter_mut().chain(info.benchmarks.iter_mut()) {
        bench.at_text = clock::local_text(bench.at);
    }
}

/// Starts a job on the PC, waking it first if it sleeps.
pub async fn start_job(state: &AppState, form: StrataJobForm) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    // A job could switch the model the code benchmark measures.
    if state.code_bench.busy() {
        return Err("A code benchmark is running. Strata jobs wait until it ends.".into());
    }
    let request = request(&form)?;
    if state.ask_pc(&agent).await.is_none() {
        wake(state, BY_UI).await?;
    }
    let job = agent.start_strata_job(&request).await?;
    state.strata.job_running.store(true, Ordering::SeqCst);
    state
        .events
        .record(EventKind::StrataJobStarted, Some(detail(&job)));
    state.overview.refresh_now();
    Ok(())
}

pub async fn cancel(state: &AppState) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    agent.cancel_strata_job().await?;
    state.overview.refresh_now();
    Ok(())
}

/// Why `wake` cannot even start, if it cannot.
pub(crate) fn cannot_wake(state: &AppState) -> Option<&'static str> {
    let cfg = state.cfg();
    if cfg.strata_url.is_none() {
        return Some(NOT_CONFIGURED);
    }
    cfg.pc_mac.is_none().then_some(NO_MAC)
}

/// Wakes the PC until its service answers, like gaming mode does; `by`
/// says why in the history.
pub(crate) async fn wake(state: &AppState, by: &str) -> Result<(), String> {
    if let Some(why) = cannot_wake(state) {
        return Err(why.into());
    }
    state
        .events
        .record(EventKind::WakeRequested, Some(by.into()));
    state.wake_until_agent().await.map_err(|e| match e {
        WakeError::NoMac => NO_MAC.to_string(),
        _ => "The PC did not wake in time.".to_string(),
    })
}

/// The request for the PC service; `Err` if the form lacks what its job
/// kind takes. No job carries a key: the PC service makes one at every start
/// of Strata.
fn request(form: &StrataJobForm) -> Result<StrataJobRequest, String> {
    let need = |what: &str| format!("The job needs a {what}.");
    let model = || form.model.clone().ok_or_else(|| need("model"));
    let context = || form.context.ok_or_else(|| need("context"));
    let preset = || form.preset.clone().ok_or_else(|| need("preset"));
    Ok(match form.kind {
        // Strata alone; with a model only for an older PC service, whose
        // page offers its own model list before Strata is set up.
        StrataJobKind::Install if form.family.is_none() && form.size.is_none() => {
            StrataJobRequest::Install {
                family: None,
                size: None,
                context: None,
                api_key: hex(&random_key()),
                experimental_speed_projection: false,
                pictures: false,
            }
        }
        StrataJobKind::Install | StrataJobKind::AddModel => {
            let family = form.family.clone().ok_or_else(|| need("family"))?;
            let size = form.size.clone().ok_or_else(|| need("size"))?;
            let experimental_speed_projection = form.speed_projection;
            let pictures = form.pictures;
            if form.kind == StrataJobKind::Install {
                StrataJobRequest::Install {
                    family: Some(family),
                    size: Some(size),
                    context: Some(context()?),
                    // The PC service ignores it; one before the key per
                    // start refuses an empty key, so it gets a random one.
                    api_key: hex(&random_key()),
                    experimental_speed_projection,
                    pictures,
                }
            } else {
                StrataJobRequest::AddModel {
                    family,
                    size,
                    context: context()?,
                    api_key: None,
                    experimental_speed_projection,
                    pictures,
                }
            }
        }
        StrataJobKind::Activate => StrataJobRequest::Activate { model: model()? },
        StrataJobKind::Delete => StrataJobRequest::Delete { model: model()? },
        StrataJobKind::Update => StrataJobRequest::Update,
        StrataJobKind::Benchmark => StrataJobRequest::Benchmark,
        StrataJobKind::SetKey => {
            return Err("A new Strata key comes with every start of Strata.".into());
        }
        StrataJobKind::SetRelay => {
            return Err("Relay mode is switched under Strata → Connection.".into());
        }
        StrataJobKind::SetSpeedProjection => StrataJobRequest::SetSpeedProjection {
            model: model()?,
            on: form.speed_projection,
        },
        StrataJobKind::SetContext => StrataJobRequest::SetContext {
            model: model()?,
            context: context()?,
        },
        StrataJobKind::SetPictures => StrataJobRequest::SetPictures {
            model: model()?,
            on: form.pictures,
        },
        StrataJobKind::CompareModels => StrataJobRequest::CompareModels,
        StrataJobKind::SwitchToTerminal => StrataJobRequest::SwitchToTerminal,
        StrataJobKind::RemoveOldStrataFolder => StrataJobRequest::RemoveOldStrataFolder,
        StrataJobKind::Tune => StrataJobRequest::Tune {
            model: model()?,
            mode: form.tune_mode,
            usage: form.tune_use,
        },
        StrataJobKind::ApplyPreset => StrataJobRequest::ApplyPreset {
            model: model()?,
            preset: preset()?,
        },
        StrataJobKind::UndoPreset => StrataJobRequest::UndoPreset {
            model: model()?,
            preset: preset()?,
        },
        StrataJobKind::SetUserArgs => StrataJobRequest::SetUserArgs {
            model: model()?,
            user_args: engine_options(form.options.as_deref().unwrap_or(""))?,
        },
        StrataJobKind::PresetOff => StrataJobRequest::PresetOff { model: model()? },
        StrataJobKind::SetEngineSettings => StrataJobRequest::SetEngineSettings {
            model: model()?,
            user_args: form.options.as_deref().map(engine_options).transpose()?,
            env: engine_env(form.env.as_deref().unwrap_or(""))?,
        },
    })
}

/// The engine environment as its field holds it: a pasted JSON object of
/// strings as it is, else one `NAME=value` per line, the name without the
/// spaces around it, the value everything after the first `=` as it is.
fn engine_env(text: &str) -> Result<BTreeMap<String, String>, String> {
    let not_a_name = |name: &str| {
        format!(
            "\"{name}\" is not a variable name (letters, digits and _, not starting with a digit)."
        )
    };
    if text.trim_start().starts_with('{') {
        let env: BTreeMap<String, String> = serde_json::from_str(text)
            .map_err(|_| "The pasted object is no JSON object of strings.".to_string())?;
        return match env.keys().find(|name| !is_env_name(name)) {
            Some(name) => Err(not_a_name(name)),
            None => Ok(env),
        };
    }
    let mut env = BTreeMap::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("Line {} is not NAME=value: {}", n + 1, line.trim()));
        };
        let name = name.trim();
        if !is_env_name(name) {
            return Err(format!("Line {}: {}", n + 1, not_a_name(name)));
        }
        if env.insert(name.to_string(), value.to_string()).is_some() {
            return Err(format!("Line {}: {name} is given twice.", n + 1));
        }
    }
    Ok(env)
}

/// The engine options as the field holds them: a pasted JSON list of
/// strings as it is, else one option per line with its values apart by
/// spaces; a value that starts with a double quote runs to the closing one
/// (`\"` and `\\` inside for a quote and a backslash).
fn engine_options(text: &str) -> Result<Vec<String>, String> {
    if text.trim_start().starts_with('[') {
        return serde_json::from_str(text)
            .map_err(|_| "The pasted list is no JSON list of strings.".to_string());
    }
    let mut args = Vec::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let words = words(line).ok_or_else(|| format!("Line {}: a quote is not closed.", n + 1))?;
        if !words[0].starts_with("--") {
            return Err(format!(
                "Line {} does not start with an option (--name): {}",
                n + 1,
                line.trim()
            ));
        }
        args.extend(words);
    }
    Ok(args)
}

/// A line's words apart by whitespace, a quoted one as one; `None` for a
/// quote that is not closed.
fn words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return Some(words);
        };
        let mut word = String::new();
        if first == '"' {
            chars.next();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' if matches!(chars.peek(), Some('"' | '\\')) => word.push(chars.next()?),
                    c => word.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                word.push(c);
            }
        }
        words.push(word);
    }
}

fn detail(job: &StrataJob) -> String {
    let what = strata_job_label(job.kind);
    match &job.model {
        Some(model) => format!("{what} {model}"),
        None => what.to_string(),
    }
}

/// Notes the job flag of a PC status; when a job ended, records how.
pub async fn note_status(state: &AppState, agent: &AgentClient, status: &PcStatus) {
    let strata = &state.strata;
    if status.strata_job {
        strata.job_running.store(true, Ordering::SeqCst);
        return;
    }
    if !strata.job_running.load(Ordering::SeqCst) {
        return;
    }
    // A status asked for before the job started says "no job" too; the job
    // itself tells. Without an answer the next status asks again.
    let Some(job) = agent.strata_job().await else {
        return;
    };
    if job.state == StrataJobState::Running || !strata.job_running.swap(false, Ordering::SeqCst) {
        return;
    }
    let (kind, detail) = match job.state {
        // What a preset job did is its own event.
        StrataJobState::Done if job.kind == StrataJobKind::ApplyPreset => (
            EventKind::PresetApplied,
            job.model.clone().unwrap_or_default(),
        ),
        StrataJobState::Done if job.kind == StrataJobKind::UndoPreset => (
            EventKind::PresetUndone,
            job.model.clone().unwrap_or_default(),
        ),
        StrataJobState::Done => (EventKind::StrataJobDone, detail(&job)),
        StrataJobState::Cancelled => (EventKind::StrataJobCancelled, detail(&job)),
        StrataJobState::Failed => (
            EventKind::StrataJobFailed,
            format!(
                "{}: {}",
                detail(&job),
                job.error.as_deref().unwrap_or("no reason given")
            ),
        ),
        StrataJobState::Interrupted | StrataJobState::Running => (
            EventKind::StrataJobFailed,
            format!("{}: interrupted", detail(&job)),
        ),
    };
    state.events.record(kind, Some(detail));
    state.overview.refresh_now();
}
