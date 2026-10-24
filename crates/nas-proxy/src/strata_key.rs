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
    /// The last status came without a generation: a PC service from before
    /// the key per start of Strata.
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

    /// Takes over `key`; gives the key held before, if there was one.
    fn take(&self, key: KeyAnswer) -> Option<String> {
        self.held.lock().unwrap().replace(key).map(|k| k.api_key)
    }
}

/// Fetches the key when the status reports another generation than the
/// one held. Not in relay mode: the PC service adds the key itself.
pub async fn note_status(state: &AppState, agent: &AgentClient, status: &PcStatus) {
    let keys = &state.strata_key;
    keys.unreported
        .store(status.strata_key_generation.is_none(), Ordering::SeqCst);
    if status.relay_on() {
        return;
    }
    if status.strata_key_generation.is_some() && status.strata_key_generation != keys.generation() {
        fetch(state, agent).await;
    }
}

/// Asks the PC service for the key again, after Strata refused `used`, the
/// key the request went out with; gives the key to send it once more with,
/// if that is another one. A status check may have fetched it meanwhile, so
/// the key held now says nothing about the refused request.
pub async fn refresh(state: &AppState, used: Option<&str>) -> Option<String> {
    fetch(state, &state.agent())
        .await
        .filter(|key| Some(key.as_str()) != used)
}

/// Fetches the key and takes it over; gives it, `None` if the PC service
/// knows none.
async fn fetch(state: &AppState, agent: &AgentClient) -> Option<String> {
    let key = agent.strata_key().await?;
    let new = key.api_key.clone();
    // The first key after a start of the proxy is no change.
    if state.strata_key.take(key).is_some_and(|old| old != new) {
        state.events.record(EventKind::StrataKeyChanged, None);
    }
    Some(new)
}
