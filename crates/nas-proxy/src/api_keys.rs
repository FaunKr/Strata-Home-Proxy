//! API keys of the clients. Every request to Strata needs one; the proxy keeps
//! only a hash of each key in `DATA_DIR`.

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    auth::{constant_time_eq, hex, random_key, unix_now},
    files::write_private,
};

const KEYS_FILE: &str = "api-keys.json";
/// Marks the proxy's keys, so they can be found in client configurations.
pub const KEY_PREFIX: &str = "shp_";
/// The last use is written to disk at most this often, so busy clients do
/// not rewrite the file with every request. A restart loses at most this much.
const LAST_USED_WRITE_SECS: u64 = 60;
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
    /// Unix time in seconds of the last request with this key.
    pub last_used: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    EmptyName,
    NameTooLong,
    DuplicateName,
    UnknownKey,
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
            Self::UnknownKey => write!(f, "Diesen Key gibt es nicht (mehr)."),
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
    #[serde(default)]
    last_used: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredFile {
    #[serde(default)]
    keys: Vec<Stored>,
}

struct Inner {
    keys: Vec<Stored>,
    /// When the last use was last written; 0 until the first write.
    last_used_written: u64,
    /// A last use changed since then.
    last_used_dirty: bool,
}

pub struct ApiKeys {
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
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
            inner: Mutex::new(Inner {
                keys,
                last_used_written: 0,
                last_used_dirty: false,
            }),
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
        let mut inner = self.inner.lock().unwrap();
        let lower = name.to_lowercase();
        if inner.keys.iter().any(|k| k.name.to_lowercase() == lower) {
            return Err(KeyError::DuplicateName);
        }
        let key = format!("{KEY_PREFIX}{}", hex(&random_key()));
        let stored = Stored {
            name: name.to_string(),
            prefix: key[..SHOWN_LEN].to_string(),
            hash: hash(&key),
            created: unix_now(),
            last_used: None,
        };
        let mut next = inner.keys.clone();
        next.push(stored.clone());
        self.save(&next)?;
        inner.keys = next;
        Ok((key, to_info(&stored)))
    }

    /// Removes a key; requests with it fail from now on.
    pub fn revoke(&self, name: &str) -> Result<(), KeyError> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.keys.clone();
        let before = next.len();
        next.retain(|k| k.name != name);
        if next.len() == before {
            return Err(KeyError::UnknownKey);
        }
        self.save(&next)?;
        inner.keys = next;
        Ok(())
    }

    /// All keys, oldest first.
    pub fn list(&self) -> Vec<KeyInfo> {
        self.inner
            .lock()
            .unwrap()
            .keys
            .iter()
            .map(to_info)
            .collect()
    }

    /// The name of the key `raw`, if it is one, and notes its use.
    pub fn check(&self, raw: &str) -> Option<String> {
        let hash = hash(raw);
        let mut inner = self.inner.lock().unwrap();
        // Compares every hash in constant time, so timing reveals nothing.
        let mut found = None;
        for (i, k) in inner.keys.iter().enumerate() {
            if constant_time_eq(k.hash.as_bytes(), hash.as_bytes()) {
                found = Some(i);
            }
        }
        let i = found?;
        let now = unix_now();
        inner.keys[i].last_used = Some(now);
        inner.last_used_dirty = true;
        let name = inner.keys[i].name.clone();
        self.write_last_used(&mut inner, now);
        Some(name)
    }

    /// Writes pending last uses once the interval has passed. Called
    /// regularly, so the last use before a quiet spell is not lost.
    pub fn flush_last_used(&self) {
        let mut inner = self.inner.lock().unwrap();
        self.write_last_used(&mut inner, unix_now());
    }

    fn write_last_used(&self, inner: &mut Inner, now: u64) {
        if !inner.last_used_dirty
            || now.saturating_sub(inner.last_used_written) < LAST_USED_WRITE_SECS
        {
            return;
        }
        inner.last_used_written = now;
        inner.last_used_dirty = false;
        if let Err(e) = self.save(&inner.keys) {
            tracing::warn!("Letzte Nutzung der API-Keys nicht gespeichert: {e}");
        }
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

fn to_info(k: &Stored) -> KeyInfo {
    KeyInfo {
        name: k.name.clone(),
        prefix: k.prefix.clone(),
        created: k.created,
        last_used: k.last_used,
    }
}

fn hash(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("strata-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_later_use_is_written_by_the_next_flush() {
        let dir = dir("keys-flush");
        let keys = ApiKeys::new(Some(dir.clone()));
        let (key, _) = keys.create("Laptop").unwrap();
        keys.check(&key);
        let first = keys.list()[0].last_used;
        // A second use inside the interval stays in memory first.
        {
            let mut inner = keys.inner.lock().unwrap();
            inner.keys[0].last_used = Some(42);
            inner.last_used_dirty = true;
        }
        keys.flush_last_used();
        let reloaded = ApiKeys::new(Some(dir.clone()));
        assert_eq!(reloaded.list()[0].last_used, first);

        keys.inner.lock().unwrap().last_used_written -= LAST_USED_WRITE_SECS;
        keys.flush_last_used();
        let reloaded = ApiKeys::new(Some(dir.clone()));
        assert_eq!(reloaded.list()[0].last_used, Some(42));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
