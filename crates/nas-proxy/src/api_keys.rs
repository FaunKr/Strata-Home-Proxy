//! API keys of the clients. Every request to Strata needs one; the proxy keeps
//! only a hash of each key in `DATA_DIR`.

use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    auth::{constant_time_eq, hex},
    files::write_private,
};

const KEYS_FILE: &str = "api-keys.json";
/// Marks the proxy's keys, so they can be found in client configurations.
pub const KEY_PREFIX: &str = "shp_";
/// How much of a key the list shows: the prefix and eight more characters.
const SHOWN_LEN: usize = KEY_PREFIX.len() + 8;
const MAX_NAME_CHARS: usize = 64;

/// A key as the API keys page shows it, without the secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub name: String,
    /// The start of the key.
    pub prefix: String,
    /// Unix time in seconds.
    pub created: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    EmptyName,
    NameTooLong,
    DuplicateName,
    NotSaved(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => write!(f, "Bitte einen Namen angeben."),
            Self::NameTooLong => write!(
                f,
                "Der Name ist zu lang (höchstens {MAX_NAME_CHARS} Zeichen)."
            ),
            Self::DuplicateName => write!(f, "Es gibt schon einen Key mit diesem Namen."),
            Self::NotSaved(e) => write!(f, "Key nicht gespeichert: {e}"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    name: String,
    prefix: String,
    /// SHA-256 of the key in hex. The keys are long and random, so a fast
    /// hash is enough; a slow password hash would only slow down requests.
    hash: String,
    created: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredFile {
    #[serde(default)]
    keys: Vec<Stored>,
}

pub struct ApiKeys {
    dir: Option<PathBuf>,
    keys: Mutex<Vec<Stored>>,
}

impl ApiKeys {
    /// Loads the keys from `data_dir`; in memory only without it.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let keys = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(KEYS_FILE)).ok())
            .and_then(|text| match serde_json::from_str::<StoredFile>(&text) {
                Ok(file) => Some(file.keys),
                Err(e) => {
                    tracing::warn!("{KEYS_FILE} nicht lesbar, wird ignoriert: {e}");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            dir: data_dir,
            keys: Mutex::new(keys),
        }
    }

    /// Whether keys survive a restart.
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    /// Creates a key; returns it in full, the only time it is ever shown.
    pub fn create(&self, name: &str) -> Result<(String, KeyInfo), KeyError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KeyError::EmptyName);
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(KeyError::NameTooLong);
        }
        let mut keys = self.keys.lock().unwrap();
        let lower = name.to_lowercase();
        if keys.iter().any(|k| k.name.to_lowercase() == lower) {
            return Err(KeyError::DuplicateName);
        }
        let key = format!("{KEY_PREFIX}{}", hex(&random_bytes()));
        let stored = Stored {
            name: name.to_string(),
            prefix: key[..SHOWN_LEN].to_string(),
            hash: hash(&key),
            created: unix_now(),
        };
        let mut next = keys.clone();
        next.push(stored.clone());
        self.save(&next)?;
        *keys = next;
        Ok((key, info(&stored)))
    }

    /// All keys, oldest first.
    pub fn list(&self) -> Vec<KeyInfo> {
        self.keys.lock().unwrap().iter().map(info).collect()
    }

    /// The name of the key `raw`, if it is one.
    pub fn check(&self, raw: &str) -> Option<String> {
        let hash = hash(raw);
        let keys = self.keys.lock().unwrap();
        // Compares every hash in constant time, so timing reveals nothing.
        let mut found = None;
        for k in keys.iter() {
            if constant_time_eq(k.hash.as_bytes(), hash.as_bytes()) {
                found = Some(k.name.clone());
            }
        }
        found
    }

    /// Writes `keys` to disk; without `DATA_DIR` there is nothing to write.
    fn save(&self, keys: &[Stored]) -> Result<(), KeyError> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let file = StoredFile {
            keys: keys.to_vec(),
        };
        let text = serde_json::to_string_pretty(&file).expect("keys serialize");
        write_private(dir, KEYS_FILE, text.as_bytes())
            .map_err(|e| KeyError::NotSaved(e.to_string()))
    }
}

fn info(k: &Stored) -> KeyInfo {
    KeyInfo {
        name: k.name.clone(),
        prefix: k.prefix.clone(),
        created: k.created,
    }
}

fn hash(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
