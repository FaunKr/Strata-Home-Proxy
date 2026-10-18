//! History of wake and sleep events, for the web UI and the power estimate.

use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

const KEEP_IN_MEMORY: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WakeSent,
    Awake,
    WakeTimeout,
    SleepAccepted,
    SleepRefused,
    GamingRejected,
    PasswordChanged,
    /// Detail: the setting's name, never its value.
    SettingSaved,
    SettingReset,
    /// The PC service reported a new MAC address (detail: the address).
    MacLearned,
    /// Detail: the key's name, never the key.
    ApiKeyCreated,
    ApiKeyRevoked,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// Unix time in seconds.
    pub ts: u64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub struct EventLog {
    recent: Mutex<VecDeque<Event>>,
    file: Option<PathBuf>,
}

impl EventLog {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let file = data_dir.and_then(|dir| match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir.join("events.jsonl")),
            Err(e) => {
                tracing::warn!("DATA_DIR {} nicht nutzbar: {e}", dir.display());
                None
            }
        });
        Self {
            recent: Mutex::new(VecDeque::new()),
            file,
        }
    }

    pub fn record(&self, kind: EventKind, detail: Option<String>) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let event = Event { ts, kind, detail };
        tracing::info!(?kind, detail = event.detail.as_deref(), "Ereignis");
        if let Some(path) = &self.file {
            let line = serde_json::to_string(&event).expect("event serializes");
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                tracing::warn!("Verlauf nicht geschrieben: {e}");
            }
        }
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == KEEP_IN_MEMORY {
            recent.pop_front();
        }
        recent.push_back(event);
    }

    pub fn recent(&self) -> Vec<Event> {
        self.recent.lock().unwrap().iter().cloned().collect()
    }
}
