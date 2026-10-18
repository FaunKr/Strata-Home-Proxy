//! The configuration in effect: each value comes from the settings page if
//! saved there, else from the environment, else the built-in default. The MAC
//! the PC reports itself beats all of these. Saved values live in `DATA_DIR`
//! and take effect at once.

use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, FIELDS, Field, find_field, format_mac, parse_mac},
    files::write_private,
};

const SETTINGS_FILE: &str = "settings.json";
const REPORTED_MAC_FILE: &str = "pc-mac";

/// Where a value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Reported by the PC service (MAC only).
    Reported,
    /// Saved on the settings page.
    Saved,
    Env,
    Default,
}

/// One value as the settings page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub field: Field,
    /// The value in effect; for secrets only whether one is set.
    pub value: Option<String>,
    pub source: Source,
    /// The value saved on the settings page, if any (never for secrets).
    pub saved: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    values: BTreeMap<String, String>,
}

struct Inner {
    saved: BTreeMap<String, String>,
    reported_mac: Option<[u8; 6]>,
    current: Arc<Config>,
}

pub struct Settings {
    /// Defaults plus environment.
    base: Config,
    env_keys: HashSet<&'static str>,
    inner: RwLock<Inner>,
}

impl Settings {
    /// `base` holds defaults and environment; `env_keys` names the values that
    /// came from the environment. Saved values are read from `base.data_dir`.
    pub fn new(base: Config, env_keys: &[&'static str]) -> Self {
        let dir = base.data_dir.clone();
        let mut saved = BTreeMap::new();
        let mut reported_mac = None;
        if let Some(dir) = &dir {
            let stored: Stored = read_file(&dir.join(SETTINGS_FILE))
                .and_then(|text| match serde_json::from_str(&text) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!("{SETTINGS_FILE} nicht lesbar, wird ignoriert: {e}");
                        None
                    }
                })
                .unwrap_or_default();
            for (key, raw) in stored.values {
                if let Err(e) = Config::default().apply(&key, &raw) {
                    tracing::warn!("Gespeicherte Einstellung {key} wird ignoriert: {e}");
                    continue;
                }
                saved.insert(key, raw);
            }
            reported_mac =
                read_file(&dir.join(REPORTED_MAC_FILE)).and_then(|text| parse_mac(&text).ok());
        }
        let current = Arc::new(resolve(&base, &saved, reported_mac));
        Self {
            base,
            env_keys: env_keys.iter().copied().collect(),
            inner: RwLock::new(Inner {
                saved,
                reported_mac,
                current,
            }),
        }
    }

    /// The configuration in effect right now.
    pub fn current(&self) -> Arc<Config> {
        Arc::clone(&self.inner.read().unwrap().current)
    }

    /// Whether saved values survive a restart.
    pub fn persistent(&self) -> bool {
        self.base.data_dir.is_some()
    }

    /// Saves one value; it takes effect at once. Invalid values change nothing.
    pub fn set(&self, key: &str, raw: &str) -> Result<()> {
        let raw = raw.trim();
        let mut check = self.current().as_ref().clone();
        check.apply(key, raw)?;
        let mut inner = self.inner.write().unwrap();
        let mut saved = inner.saved.clone();
        saved.insert(key.to_string(), raw.to_string());
        self.commit(&mut inner, saved)
    }

    /// Saves several values together: all take effect at once, or, if one is
    /// invalid, none does. The error names the invalid value's key, or none
    /// if saving itself failed.
    pub fn set_many(&self, values: &[(&str, &str)]) -> Result<(), (Option<String>, anyhow::Error)> {
        let mut check = self.current().as_ref().clone();
        for (key, raw) in values {
            check
                .apply(key, raw.trim())
                .map_err(|e| (Some(key.to_string()), e))?;
        }
        let mut inner = self.inner.write().unwrap();
        let mut saved = inner.saved.clone();
        for (key, raw) in values {
            saved.insert(key.to_string(), raw.trim().to_string());
        }
        self.commit(&mut inner, saved).map_err(|e| (None, e))
    }

    /// Drops a saved value, so the environment or default applies again.
    pub fn reset(&self, key: &str) -> Result<()> {
        find_field(key).ok_or_else(|| anyhow::anyhow!("unknown setting {key}"))?;
        let mut inner = self.inner.write().unwrap();
        let mut saved = inner.saved.clone();
        if saved.remove(key).is_none() {
            return Ok(());
        }
        self.commit(&mut inner, saved)
    }

    /// The MAC the PC service reported last, if any.
    pub fn reported_mac(&self) -> Option<[u8; 6]> {
        self.inner.read().unwrap().reported_mac
    }

    /// Takes the MAC the PC service reported. Returns true if it is new.
    pub fn set_reported_mac(&self, mac: [u8; 6]) -> bool {
        let mut inner = self.inner.write().unwrap();
        if inner.reported_mac == Some(mac) {
            return false;
        }
        inner.reported_mac = Some(mac);
        if let Some(dir) = &self.base.data_dir
            && let Err(e) = write_private(dir, REPORTED_MAC_FILE, format_mac(mac).as_bytes())
        {
            tracing::warn!("Gemeldete MAC nicht gespeichert: {e}");
        }
        inner.current = Arc::new(resolve(&self.base, &inner.saved, inner.reported_mac));
        true
    }

    /// All values with their origin, in page order.
    pub fn entries(&self) -> Vec<Entry> {
        let inner = self.inner.read().unwrap();
        FIELDS
            .iter()
            .map(|field| {
                let key = field.key;
                let source = if key == "PC_MAC" && inner.reported_mac.is_some() {
                    Source::Reported
                } else if inner.saved.contains_key(key) {
                    Source::Saved
                } else if self.env_keys.contains(key) {
                    Source::Env
                } else {
                    Source::Default
                };
                let mut value = inner.current.value(key);
                let mut saved = inner.saved.get(key).cloned();
                if field.secret {
                    value = value.map(|_| "set".to_string());
                    saved = None;
                }
                Entry {
                    field: *field,
                    value,
                    source,
                    saved,
                }
            })
            .collect()
    }

    /// Writes `saved` to disk first and takes it over only if that worked, so
    /// the page never reports a value as saved that a restart would lose.
    fn commit(&self, inner: &mut Inner, saved: BTreeMap<String, String>) -> Result<()> {
        if let Some(dir) = &self.base.data_dir {
            let stored = Stored {
                values: saved.clone(),
            };
            let text = serde_json::to_string_pretty(&stored).expect("settings serialize");
            write_private(dir, SETTINGS_FILE, text.as_bytes())
                .map_err(|e| anyhow::anyhow!("not saved: {e}"))?;
        }
        inner.current = Arc::new(resolve(&self.base, &saved, inner.reported_mac));
        inner.saved = saved;
        Ok(())
    }
}

fn resolve(
    base: &Config,
    saved: &BTreeMap<String, String>,
    reported_mac: Option<[u8; 6]>,
) -> Config {
    let mut cfg = base.clone();
    for (key, raw) in saved {
        // Checked when saved or loaded.
        let _ = cfg.apply(key, raw);
    }
    if reported_mac.is_some() {
        cfg.pc_mac = reported_mac;
    }
    cfg
}

fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn base(dir: Option<PathBuf>) -> Config {
        let mut cfg = Config::default();
        cfg.apply("STRATA_URL", "http://env:8080").unwrap();
        cfg.data_dir = dir;
        cfg
    }

    fn entry<'a>(entries: &'a [Entry], key: &str) -> &'a Entry {
        entries.iter().find(|e| e.field.key == key).unwrap()
    }

    #[test]
    fn saved_beats_env_and_reset_restores_it() {
        let s = Settings::new(base(None), &["STRATA_URL"]);
        assert_eq!(entry(&s.entries(), "STRATA_URL").source, Source::Env);
        assert_eq!(
            entry(&s.entries(), "IDLE_TIMEOUT_SECS").source,
            Source::Default
        );
        s.set("STRATA_URL", "http://saved:8080").unwrap();
        assert_eq!(s.current().strata_url.as_deref(), Some("http://saved:8080"));
        assert_eq!(entry(&s.entries(), "STRATA_URL").source, Source::Saved);
        s.reset("STRATA_URL").unwrap();
        assert_eq!(s.current().strata_url.as_deref(), Some("http://env:8080"));
        assert_eq!(entry(&s.entries(), "STRATA_URL").source, Source::Env);
    }

    #[test]
    fn invalid_values_change_nothing() {
        let s = Settings::new(base(None), &[]);
        assert!(s.set("IDLE_TIMEOUT_SECS", "bald").is_err());
        assert!(s.set("DATA_DIR", "/tmp").is_err());
        assert_eq!(
            entry(&s.entries(), "IDLE_TIMEOUT_SECS").source,
            Source::Default
        );
    }

    #[test]
    fn reported_mac_beats_saved_mac() {
        let s = Settings::new(base(None), &[]);
        s.set("PC_MAC", "01:02:03:04:05:06").unwrap();
        assert_eq!(s.current().pc_mac, Some([1, 2, 3, 4, 5, 6]));
        assert!(s.set_reported_mac([9; 6]));
        assert!(!s.set_reported_mac([9; 6]));
        assert_eq!(s.current().pc_mac, Some([9; 6]));
        let mac = entry(&s.entries(), "PC_MAC").clone();
        assert_eq!(mac.source, Source::Reported);
        assert_eq!(mac.saved.as_deref(), Some("01:02:03:04:05:06"));
    }

    #[test]
    fn secrets_are_not_shown() {
        let s = Settings::new(base(None), &[]);
        s.set("PC_AGENT_TOKEN", "geheim").unwrap();
        let token = entry(&s.entries(), "PC_AGENT_TOKEN").clone();
        assert_eq!(token.value.as_deref(), Some("set"));
        assert_eq!(token.saved, None);
        assert_eq!(s.current().pc_agent_token.as_deref(), Some("geheim"));
    }

    #[test]
    fn saved_values_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("strata-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::new(base(Some(dir.clone())), &[]);
        assert!(s.persistent());
        s.set("IDLE_TIMEOUT_SECS", "60").unwrap();
        s.set_reported_mac([7; 6]);
        let again = Settings::new(base(Some(dir.clone())), &[]);
        assert_eq!(again.current().idle_timeout.as_secs(), 60);
        assert_eq!(again.current().pc_mac, Some([7; 6]));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
