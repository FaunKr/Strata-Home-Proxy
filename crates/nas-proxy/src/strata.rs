//! Strata on the PC through its service: the Strata page, jobs started from
//! it, and the Strata key. The proxy makes the key, hands it to the PC with
//! the job and uses it itself; it never reaches the page.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

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
    state::{AppState, WakeError},
};

const BY_UI: &str = "für Strata, über Weboberfläche";
const ASLEEP: &str = "Der PC schläft oder sein Dienst antwortet nicht. Ein Knopf weckt ihn.";
const TOO_OLD: &str =
    "Der PC-Dienst kennt die Strata-Verwaltung nicht. Bitte den PC-Dienst aktualisieren.";

#[derive(Default)]
pub struct StrataState {
    /// The last answer of the PC service, shown while the PC sleeps.
    last: Mutex<Option<StrataInfo>>,
    /// The PC service reported a running job at its last status.
    job_running: AtomicBool,
    /// A new Strata key the proxy uses once the PC took it over.
    pending_key: Mutex<Option<String>>,
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
    let body = body(&form, key.as_deref());
    let job = agent.start_strata_job(&body).await?;
    state.strata.job_running.store(true, Ordering::SeqCst);
    match (form.kind, key) {
        // Strata gets the key with the setup; requests use it from now on.
        (StrataJobKind::Install, Some(key)) => state
            .settings
            .set("STRATA_API_KEY", &key)
            .map_err(|e| format!("Strata-Key nicht gespeichert: {e}"))?,
        // Strata takes the key over when the job is done; until then the old one holds.
        (StrataJobKind::SetKey, Some(key)) => {
            *state.strata.pending_key.lock().unwrap() = Some(key);
        }
        _ => {}
    }
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
/// takes over a new Strata key.
pub async fn note_status(state: &AppState, agent: &AgentClient, status: &PcStatus) {
    let was = state
        .strata
        .job_running
        .swap(status.strata_job, Ordering::SeqCst);
    if !was || status.strata_job {
        return;
    }
    let pending = state.strata.pending_key.lock().unwrap().take();
    let Some(job) = agent.strata_job().await else {
        return;
    };
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
        StrataJobState::Interrupted => (
            EventKind::StrataJobFailed,
            format!("{}: unterbrochen", detail(&job)),
        ),
        StrataJobState::Running => return,
    };
    state.events.record(kind, Some(detail));
    if job.kind == StrataJobKind::SetKey
        && job.state == StrataJobState::Done
        && let Some(key) = pending
    {
        match state.settings.set("STRATA_API_KEY", &key) {
            Ok(()) => state.events.record(EventKind::StrataKeyChanged, None),
            Err(e) => tracing::error!("Neuer Strata-Key nicht gespeichert: {e}"),
        }
    }
    state.overview.refresh_now();
}
