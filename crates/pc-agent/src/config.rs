//! Configuration file `strata-pc-agent.toml`, next to the program by default.

use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address the service listens on.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Shared secret with the NAS proxy (`PC_AGENT_TOKEN` there).
    pub token: String,
    /// Only these addresses may call the service; localhost is always allowed
    /// (for the desktop shortcut). Empty means everyone with the token.
    #[serde(default)]
    pub allowed_clients: Vec<IpAddr>,
    /// Strata as seen from the PC itself.
    #[serde(default = "default_strata_url")]
    pub strata_url: String,
    /// Strata's API key, if it needs one for `/health` and `/slots`.
    #[serde(default)]
    pub strata_api_key: Option<String>,
    /// What starts Strata, usually the path to `START-HERE.bat`.
    pub strata_start: PathBuf,
    /// Extra arguments for `strata_start`.
    #[serde(default)]
    pub strata_args: Vec<String>,
    /// Process names that make up a running Strata; ended in gaming mode and
    /// before a restart. Check Task Manager while Strata runs.
    pub strata_processes: Vec<String>,
    /// No sleep while the last keyboard or mouse input is more recent than this.
    #[serde(default = "default_user_idle")]
    pub user_idle_minutes: u64,
    /// Start Strata on its own when it is not running and gaming mode is off.
    #[serde(default = "default_true")]
    pub auto_start: bool,
    /// Strata that has not answered `/health` for this long is restarted.
    #[serde(default = "default_unhealthy")]
    pub restart_after_secs: u64,
    /// After a start, give Strata this long to load the model before checking again.
    #[serde(default = "default_load")]
    pub load_time_secs: u64,
    /// Folder for the gaming flag and the log; next to the program by default.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8091".parse().unwrap()
}
fn default_strata_url() -> String {
    "http://127.0.0.1:8080".into()
}
fn default_user_idle() -> u64 {
    10
}
fn default_true() -> bool {
    true
}
fn default_unhealthy() -> u64 {
    60
}
fn default_load() -> u64 {
    240
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Konfiguration {} nicht lesbar", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("Konfiguration {} ist fehlerhaft", path.display()))?;
        cfg.strata_url = cfg.strata_url.trim_end_matches('/').to_string();
        if cfg.state_dir.is_none() {
            cfg.state_dir = path.parent().map(Path::to_path_buf);
        }
        Ok(cfg)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.clone().unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn user_idle(&self) -> Duration {
        Duration::from_secs(self.user_idle_minutes * 60)
    }

    /// Where the desktop shortcut reaches the running service.
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.listen.port())
    }
}

/// `strata-pc-agent.toml` next to the program.
pub fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("strata-pc-agent.toml")))
        .unwrap_or_else(|| PathBuf::from("strata-pc-agent.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("strata-pc-agent.example.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.listen.port(), 8091);
        assert_eq!(cfg.user_idle(), Duration::from_secs(600));
        assert!(!cfg.strata_processes.is_empty());
    }
}
