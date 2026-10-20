//! The Strata key: the PC service makes a new one before every start of
//! Strata and writes it into every model config, where Strata reads it. The
//! proxy learns it from `GET /strata/key` whenever the generation in the
//! status changes. Until the first start the key is the one the configs
//! already hold, so a service that restarts beside a running Strata still
//! knows it.

use std::sync::Mutex;

use strata_pc_protocol::StrataKey as KeyAnswer;

use crate::{config::Config, install::Installation};

#[derive(Default)]
pub(crate) struct StrataKey(Mutex<Held>);

#[derive(Default)]
struct Held {
    /// `None` until a start made one or a model config was seen with one.
    key: Option<String>,
    generation: u64,
}

impl StrataKey {
    /// Starts the generations at a random value, so a restarted service
    /// never reports the generation of the one before.
    pub(crate) fn new() -> Self {
        let generation = u64::from_le_bytes(random_bytes());
        Self(Mutex::new(Held {
            key: None,
            generation,
        }))
    }

    /// The key and its generation; `None` while no model config holds one.
    pub(crate) fn current(&self, cfg: &Config) -> Option<KeyAnswer> {
        let mut held = self.0.lock().unwrap();
        if held.key.is_none() {
            held.key = Installation::of(cfg).key();
        }
        let api_key = held.key.clone()?;
        Some(KeyAnswer {
            api_key,
            generation: held.generation,
        })
    }

    pub(crate) fn key(&self, cfg: &Config) -> Option<String> {
        self.current(cfg).map(|k| k.api_key)
    }

    /// Makes a new key and writes it into every model config; without
    /// model configs there is nothing to write and Strata starts without a
    /// key the service manages. `Err` leaves every config and the key as
    /// they were.
    pub(crate) fn renew(&self, cfg: &Config) -> Result<(), String> {
        let install = Installation::of(cfg);
        if install.models().is_empty() {
            return Ok(());
        }
        let key = hex(&random_bytes::<32>());
        install.set_key(&key)?;
        let mut held = self.0.lock().unwrap();
        held.key = Some(key);
        held.generation = held.generation.wrapping_add(1);
        Ok(())
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the system has randomness");
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
