//! The Strata key, as the PC service makes it at every start of Strata (see
//! `docs/pc-agent-api.md`). The proxy fetches it when the generation in the
//! PC's status changes, or when Strata refuses the key it has, and keeps it
//! in memory only: after a restart of the proxy the next status brings it.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use strata_pc_protocol::{PcStatus, StrataKey as KeyAnswer};

use crate::{agent::AgentClient, events::EventKind, state::AppState};

#[derive(Default)]
pub struct StrataKey {
    held: Mutex<Option<KeyAnswer>>,
    /// The last status came without a generation: an older PC service, or
    /// one that knows no key yet.
    unreported: AtomicBool,
}

impl StrataKey {
    pub fn key(&self) -> Option<String> {
        self.held
            .lock()
            .unwrap()
            .as_ref()
            .map(|k| k.api_key.clone())
    }

    /// The PC service reported no key generation last time.
    pub fn unreported(&self) -> bool {
        self.unreported.load(Ordering::SeqCst)
    }

    fn generation(&self) -> Option<u64> {
        self.held.lock().unwrap().as_ref().map(|k| k.generation)
    }

    /// Takes over `key`; true if it differs from the one held.
    fn take(&self, key: KeyAnswer) -> bool {
        let mut held = self.held.lock().unwrap();
        let changed = held.as_ref().is_some_and(|h| h.api_key != key.api_key);
        *held = Some(key);
        changed
    }
}

/// Fetches the key when the status reports another generation than the
/// one held.
pub async fn note_status(state: &AppState, agent: &AgentClient, status: &PcStatus) {
    let keys = &state.strata_key;
    keys.unreported
        .store(status.strata_key_generation.is_none(), Ordering::SeqCst);
    if status.strata_key_generation.is_some() && status.strata_key_generation != keys.generation() {
        fetch(state, agent).await;
    }
}

/// Asks the PC service for the key again, after Strata refused the one held;
/// true if the key changed.
pub async fn refresh(state: &AppState) -> bool {
    fetch(state, &state.agent()).await
}

async fn fetch(state: &AppState, agent: &AgentClient) -> bool {
    let Some(key) = agent.strata_key().await else {
        return false;
    };
    let changed = state.strata_key.take(key);
    if changed {
        state.events.record(EventKind::StrataKeyChanged, None);
    }
    changed
}
