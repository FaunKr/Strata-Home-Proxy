//! API keys of the clients. Every request to Strata needs one; the proxy keeps
//! only a hash of each key in `DATA_DIR`.

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    auth::{constant_time_eq, hex, random_key},
    clock::unix_now,
    files::write_private,
};

const KEYS_FILE: &str = "api-keys.json";
/// Marks the proxy's keys, so they can be found in client configurations.
pub const KEY_PREFIX: &str = "shp_";
/// The last use is written to disk at most this often, by the idle loop, so
/// requests never wait for the disk. A restart loses at most about this much.
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
            Self::EmptyName => write!(f, "Please enter a name."),
            Self::NameTooLong => write!(
                f,
                "The name is too long (at most {MAX_NAME_CHARS} characters)."
            ),
            Self::DuplicateName => write!(f, "A key with this name exists already."),
            Self::UnknownKey => write!(f, "This key does not exist (any more)."),
            Self::NotSaved(e) => write!(f, "Key not saved: {e}"),
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
    /// Why the key file could not be read. Nothing is written then, so a
    /// damaged file is never replaced by one without the keys it holds.
    broken: Option<String>,
    /// Held for every write, taken before `inner`. One writer at a time, so a
    /// flush never writes an older list over a create or revoke, and two
    /// writes never share the temp file.
    saving: Mutex<()>,
    inner: Mutex<Inner>,
}

impl ApiKeys {
    /// Loads the keys from `data_dir`; in memory only without it.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let (keys, broken) = match data_dir.as_ref().map(|dir| load(&dir.join(KEYS_FILE))) {
            None | Some(Ok(None)) => (Vec::new(), None),
            Some(Ok(Some(keys))) => (keys, None),
            Some(Err(e)) => {
                tracing::error!(
                    "{KEYS_FILE} nicht lesbar ({e}); bis zur Reparatur gilt kein Key und es wird nichts gespeichert"
                );
                (Vec::new(), Some(e))
            }
        };
        Self {
            dir: data_dir,
            broken,
            saving: Mutex::new(()),
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
        let _saving = self.saving.lock().unwrap();
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

    /// Removes a key; requests with it fail from now on. Names match as on
    /// creation: trimmed and regardless of case. The prefix must match too,
    /// so a page shown before a key was replaced never revokes the new one.
    pub fn revoke(&self, name: &str, prefix: &str) -> Result<(), KeyError> {
        let lower = name.trim().to_lowercase();
        let _saving = self.saving.lock().unwrap();
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.keys.clone();
        let before = next.len();
        next.retain(|k| k.name.to_lowercase() != lower || k.prefix != prefix);
        if next.len() == before {
            return Err(KeyError::UnknownKey);
        }
        self.save(&next)?;
        inner.keys = next;
        Ok(())
    }

    /// All keys, oldest first.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().keys.is_empty()
    }

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
        inner.keys[i].last_used = Some(unix_now());
        inner.last_used_dirty = true;
        Some(inner.keys[i].name.clone())
    }

    /// Writes pending last uses once the interval has passed. Called by the
    /// idle loop, so the last use before a quiet spell is not lost. The file
    /// is written outside the key lock; requests checking keys never wait for it.
    pub fn flush_last_used(&self) {
        let now = unix_now();
        let _saving = self.saving.lock().unwrap();
        let keys = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.last_used_dirty
                || now.saturating_sub(inner.last_used_written) < LAST_USED_WRITE_SECS
            {
                return;
            }
            inner.last_used_written = now;
            inner.last_used_dirty = false;
            inner.keys.clone()
        };
        if let Err(e) = self.save(&keys) {
            tracing::warn!("Letzte Nutzung der API-Keys nicht gespeichert: {e}");
            // Try again with the next flush.
            self.inner.lock().unwrap().last_used_dirty = true;
        }
    }

    /// Writes `keys` to disk; without `DATA_DIR` there is nothing to write.
    fn save(&self, keys: &[Stored]) -> Result<(), KeyError> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        if let Some(e) = &self.broken {
            return Err(KeyError::NotSaved(format!(
                "{KEYS_FILE} ist beschädigt ({e}). Bitte reparieren oder löschen und den Proxy neu starten."
            )));
        }
        let file = StoredFile {
            keys: keys.to_vec(),
        };
        let text = serde_json::to_string_pretty(&file).expect("keys serialize");
        write_private(dir, KEYS_FILE, text.as_bytes())
            .map_err(|e| KeyError::NotSaved(e.to_string()))
    }
}

/// The keys in `path`; `None` if there is no such file yet.
fn load(path: &std::path::Path) -> Result<Option<Vec<Stored>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str::<StoredFile>(&text)
        .map(|file| Some(file.keys))
        .map_err(|e| e.to_string())
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
    fn uses_are_written_by_the_flush_not_by_the_request() {
        let dir = dir("keys-flush");
        let keys = ApiKeys::new(Some(dir.clone()));
        let (key, _) = keys.create("Laptop").unwrap();
        keys.check(&key);
        let used = keys.list()[0].last_used;
        assert!(used.is_some());
        assert_eq!(ApiKeys::new(Some(dir.clone())).list()[0].last_used, None);

        keys.flush_last_used();
        assert_eq!(ApiKeys::new(Some(dir.clone())).list()[0].last_used, used);

        // A later use inside the interval waits for the next due flush.
        {
            let mut inner = keys.inner.lock().unwrap();
            inner.keys[0].last_used = Some(42);
            inner.last_used_dirty = true;
        }
        keys.flush_last_used();
        assert_eq!(ApiKeys::new(Some(dir.clone())).list()[0].last_used, used);
        keys.inner.lock().unwrap().last_used_written -= LAST_USED_WRITE_SECS;
        keys.flush_last_used();
        assert_eq!(
            ApiKeys::new(Some(dir.clone())).list()[0].last_used,
            Some(42)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_damaged_file_is_never_overwritten() {
        let dir = dir("keys-broken");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(KEYS_FILE);
        write_private(&dir, KEYS_FILE, b"{\"keys\": [ kaputt").unwrap();
        let keys = ApiKeys::new(Some(dir.clone()));
        assert!(keys.list().is_empty());
        assert!(matches!(keys.create("Neu"), Err(KeyError::NotSaved(_))));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"keys\": [ kaputt"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn revoking_matches_names_like_creating() {
        let keys = ApiKeys::new(None);
        let (key, info) = keys.create("Laptop").unwrap();
        keys.revoke("  laptop ", &info.prefix).unwrap();
        assert_eq!(keys.check(&key), None);
    }
}
