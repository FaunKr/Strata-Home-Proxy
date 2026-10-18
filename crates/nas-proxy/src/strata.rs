//! Strata on the PC through its service: the Strata page, jobs started from
//! it, and the Strata key. The proxy makes the key, hands it to the PC with
//! the job and uses it itself once the job is done; it never reaches the page.

use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_admin_ui::{
    api::{StrataInfo, StrataJob, StrataJobForm, StrataJobKind, StrataJobState, StrataPage},
    labels::{NO_AGENT, NO_MAC, NOT_CONFIGURED, strata_job_label},
};

use crate::{
    agent::{AgentClient, PcStatus},
    auth::{hex, random_key},
    clock,
    events::EventKind,
    files::write_private,
    state::{AppState, WakeError},
};

/// A key handed to the PC that counts once its job is done; kept over a
/// restart of the proxy.
const PENDING_FILE: &str = "strata-pending-key.json";
const BY_UI: &str = "für Strata, über Weboberfläche";
const ASLEEP: &str = "Der PC schläft oder sein Dienst antwortet nicht. Ein Knopf weckt ihn.";
const TOO_OLD: &str =
    "Der PC-Dienst kennt die Strata-Verwaltung nicht. Bitte den PC-Dienst aktualisieren.";

pub struct StrataState {
    /// The last answer of the PC service, shown while the PC sleeps.
    last: Mutex<Option<StrataInfo>>,
    /// A job was started or reported running, and its end is not noted yet.
    job_running: AtomicBool,
    /// A new Strata key the proxy uses once the PC took it over.
    pending: Mutex<Option<PendingKey>>,
    dir: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingKey {
    /// The job that hands the key to Strata.
    job: u64,
    key: String,
}

impl StrataState {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let pending: Option<PendingKey> = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(PENDING_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self {
            last: Mutex::new(None),
            job_running: AtomicBool::new(pending.is_some()),
            pending: Mutex::new(pending),
            dir: data_dir,
        }
    }

    fn set_pending(&self, pending: Option<PendingKey>) {
        if let Some(dir) = &self.dir {
            let written = match &pending {
                Some(p) => write_private(
                    dir,
                    PENDING_FILE,
                    &serde_json::to_vec(p).expect("pending key serializes"),
                ),
                None => std::fs::remove_file(dir.join(PENDING_FILE)).or_else(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(e),
                }),
            };
            if let Err(e) = written {
                tracing::error!("{PENDING_FILE} nicht gespeichert: {e}");
            }
        }
        *self.pending.lock().unwrap() = pending;
    }
}

/// The Strata page; asks the PC service but never wakes the PC.
pub async fn page(state: &AppState) -> StrataPage {
    let agent = state.agent();
    let last = || state.strata.last.lock().unwrap().clone();
    if !agent.configured() {
        return StrataPage {
            problem: Some(NO_AGENT.into()),
            gaming: false,
            info: None,
            stale: false,
        };
    }
    let Some(status) = state.pc_status(&agent).await else {
        let info = last();
        return StrataPage {
            problem: Some(ASLEEP.into()),
            gaming: false,
            stale: info.is_some(),
            info,
        };
    };
    let Some(mut info) = agent.strata().await else {
        return StrataPage {
            problem: Some(TOO_OLD.into()),
            gaming: status.gaming_mode,
            info: None,
            stale: false,
        };
    };
    if let Some(bench) = info.benchmark.as_mut() {
        bench.at_text = clock::local_text(bench.at);
    }
    *state.strata.last.lock().unwrap() = Some(info.clone());
    StrataPage {
        problem: None,
        gaming: status.gaming_mode,
        info: Some(info),
        stale: false,
    }
}

/// Starts a job on the PC, waking it first if it sleeps.
pub async fn start_job(state: &AppState, form: StrataJobForm) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    if state.pc_status(&agent).await.is_none() {
        wake(state).await?;
    }
    let key = matches!(form.kind, StrataJobKind::Install | StrataJobKind::SetKey)
        .then(|| hex(&random_key()));
    // Another model gets the key the proxy uses now.
    let current = (form.kind == StrataJobKind::AddModel)
        .then(|| state.cfg().strata_api_key.clone())
        .flatten();
    let body = body(&form, key.as_deref().or(current.as_deref()));
    let job = agent.start_strata_job(&body).await?;
    // Strata has the key once the job is done; until then the old one holds.
    // The key first: whoever then sees the job end takes it over.
    if let Some(key) = key {
        state.strata.set_pending(Some(PendingKey {
            job: job.id.unwrap_or_default(),
            key,
        }));
    }
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

/// Wakes the PC until its service answers, like gaming mode does.
async fn wake(state: &AppState) -> Result<(), String> {
    let cfg = state.cfg();
    if cfg.strata_url.is_none() {
        return Err(NOT_CONFIGURED.into());
    }
    if cfg.pc_mac.is_none() {
        return Err(NO_MAC.into());
    }
    state
        .events
        .record(EventKind::WakeRequested, Some(BY_UI.into()));
    state.wake_until_agent().await.map_err(|e| match e {
        WakeError::NoMac => NO_MAC.to_string(),
        _ => "Der PC ist nicht rechtzeitig aufgewacht.".to_string(),
    })
}

/// The request for the PC service; only fields its job kind takes.
fn body(form: &StrataJobForm, key: Option<&str>) -> serde_json::Value {
    let mut body = json!({ "kind": form.kind });
    let mut put = |name: &str, value: serde_json::Value| {
        body[name] = value;
    };
    match form.kind {
        StrataJobKind::Install | StrataJobKind::AddModel => {
            put("family", json!(form.family));
            put("size", json!(form.size));
            put("context", json!(form.context));
        }
        StrataJobKind::Activate | StrataJobKind::Delete => put("model", json!(form.model)),
        StrataJobKind::Update | StrataJobKind::Benchmark | StrataJobKind::SetKey => {}
    }
    if let Some(key) = key {
        put("api_key", json!(key));
    }
    body
}

fn detail(job: &StrataJob) -> String {
    let what = strata_job_label(job.kind);
    match &job.model {
        Some(model) => format!("{what} {model}"),
        None => what.to_string(),
    }
}

/// Notes the job flag of a PC status; when a job ended, records how and
/// takes over the Strata key it handed to Strata.
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
        StrataJobState::Done => (EventKind::StrataJobDone, detail(&job)),
        StrataJobState::Cancelled => (EventKind::StrataJobCancelled, detail(&job)),
        StrataJobState::Failed => (
            EventKind::StrataJobFailed,
            format!(
                "{}: {}",
                detail(&job),
                job.error.as_deref().unwrap_or("ohne Grund")
            ),
        ),
        StrataJobState::Interrupted | StrataJobState::Running => (
            EventKind::StrataJobFailed,
            format!("{}: unterbrochen", detail(&job)),
        ),
    };
    state.events.record(kind, Some(detail));
    let pending = strata.pending.lock().unwrap().clone();
    if let Some(pending) = pending {
        // A key whose job failed or was replaced never reached Strata.
        if job.id.unwrap_or_default() == pending.job && job.state == StrataJobState::Done {
            match state.settings.set("STRATA_API_KEY", &pending.key) {
                Ok(()) => state.events.record(EventKind::StrataKeyChanged, None),
                Err(e) => tracing::error!("Neuer Strata-Key nicht gespeichert: {e}"),
            }
        }
        strata.set_pending(None);
    }
    state.overview.refresh_now();
}
