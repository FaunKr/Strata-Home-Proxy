//! History of wake and sleep events, for the web UI and the power estimate.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const KEEP_IN_MEMORY: usize = 1000;

pub use strata_admin_ui::api::EventKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
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
        // The history so far, so the overview shows it after a restart too.
        let recent = file.as_deref().map(load).unwrap_or_default();
        Self {
            recent: Mutex::new(recent),
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

    /// The latest `n` events about the PC, newest first.
    pub fn latest_pc(&self, n: usize) -> Vec<Event> {
        self.recent
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|e| !e.kind.is_admin())
            .take(n)
            .cloned()
            .collect()
    }
}

/// The last `KEEP_IN_MEMORY` events from `events.jsonl`; lines that cannot be
/// read, e.g. of kinds this version does not know, are skipped.
fn load(path: &Path) -> VecDeque<Event> {
    let Ok(file) = std::fs::File::open(path) else {
        return VecDeque::new();
    };
    let mut recent = VecDeque::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(event) = serde_json::from_str::<Event>(&line) {
            if recent.len() == KEEP_IN_MEMORY {
                recent.pop_front();
            }
            recent.push_back(event);
        }
    }
    recent
}
