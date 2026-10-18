//! Values the service sets itself once it manages the Strata installation:
//! start script, Strata's programs and its API key. They live in
//! `strata-managed.json` next to the configuration and come before the values
//! in the TOML, which stays as the owner wrote it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const MANAGED_FILE: &str = "strata-managed.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata_start: Option<PathBuf>,
    /// Arguments for `strata_start`; only with it, never for the TOML's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strata_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata_processes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata_api_key: Option<String>,
}

impl Managed {
    /// The managed values in `dir`; none if there are none yet. A damaged
    /// file is reported and ignored, so the TOML still applies.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(MANAGED_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::error!("{} nicht lesbar, gilt die TOML: {e}", path.display());
                return Self::default();
            }
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::error!("{} ist fehlerhaft, gilt die TOML: {e}", path.display());
            Self::default()
        })
    }

    /// Writes the values to `dir`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).expect("managed values serialize");
        crate::write_replacing(&dir.join(MANAGED_FILE), &text)
            .with_context(|| format!("{MANAGED_FILE} nicht gespeichert"))
    }

    /// What starts Strata, and with which arguments.
    pub fn start<'a>(&'a self, cfg: &'a Config) -> (&'a Path, &'a [String]) {
        match &self.strata_start {
            Some(start) => (start, &self.strata_args),
            None => (&cfg.strata_start, &cfg.strata_args),
        }
    }

    pub fn processes<'a>(&'a self, cfg: &'a Config) -> &'a [String] {
        self.strata_processes
            .as_deref()
            .unwrap_or(&cfg.strata_processes)
    }

    pub fn api_key<'a>(&'a self, cfg: &'a Config) -> Option<&'a str> {
        self.strata_api_key
            .as_deref()
            .or(cfg.strata_api_key.as_deref())
    }
}
