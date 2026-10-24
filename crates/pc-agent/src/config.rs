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
    /// Ignored with a warning: the service makes a new Strata key at every
    /// start of Strata. Kept so an older configuration still loads.
    #[serde(default)]
    pub strata_api_key: Option<String>,
    /// What starts Strata, usually a `.bat` in Strata's folder. Empty after the
    /// one-line setup until the Strata page sets it; without it the service
    /// never starts Strata itself.
    #[serde(default)]
    pub strata_start: Option<PathBuf>,
    /// Extra arguments for `strata_start`.
    #[serde(default)]
    pub strata_args: Vec<String>,
    /// Process names that make up a running Strata; ended in gaming mode and
    /// before a restart. Check Task Manager while Strata runs.
    #[serde(default)]
    pub strata_processes: Vec<String>,
    /// No sleep while the last keyboard or mouse input is more recent than this.
    #[serde(default = "default_user_idle")]
    pub user_idle_minutes: u64,
    /// Start Strata on its own when it is not running and gaming mode is off.
    #[serde(default = "default_true")]
    pub auto_start: bool,
    /// End Strata before standby and start it again after waking. Slower to
    /// wake (the model loads again, 1–3 min), but the GPU starts clean; for
    /// drivers that hang on the first request after standby.
    #[serde(default)]
    pub fresh_strata_after_standby: bool,
    /// Strata that has not answered `/health` for this long is restarted.
    #[serde(default = "default_unhealthy")]
    pub restart_after_secs: u64,
    /// After a start, give Strata this long to load the model before checking again.
    #[serde(default = "default_load")]
    pub load_time_secs: u64,
    /// After a context switch the active model must answer within this
    /// long, else its old context comes back.
    #[serde(default = "default_context_start")]
    pub context_start_secs: u64,
    /// Pause between ending Strata and starting it again, so the ended one
    /// frees its port first.
    #[serde(default = "default_stop_settle")]
    pub stop_settle_ms: u64,
    /// How long a forced sleep or forced Strata restart waits for the job
    /// it cancelled to end; after that it goes ahead anyway. The NAS waits
    /// longer for the answer (`force_timeout` there).
    #[serde(default = "default_cancel_wait")]
    pub cancel_wait_secs: u64,
    /// How long an update waits for the Strata it stopped to stop answering;
    /// one still answering then refuses the update.
    #[serde(default = "default_update_stop_wait")]
    pub update_stop_wait_secs: u64,
    /// Folder for the gaming flag and the log; next to the program by default.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    /// MAC address reported to the NAS instead of the one found on the network
    /// card the NAS connects to (`AA:BB:CC:DD:EE:FF` or `AA-BB-…`).
    #[serde(default)]
    pub mac: Option<String>,
    /// The NAS as the one-line setup reached it (`http://<NAS>:<Port>`);
    /// where updates of this program come from.
    #[serde(default)]
    pub nas_url: Option<String>,
    /// Where an older Strata folder was fetched from with Git; its updates
    /// still pull from there. Fixed here, never taken from a request.
    #[serde(default = "default_strata_repo")]
    pub strata_repo: String,
    /// The Strata folder of an older setup from source (a Git clone); a new
    /// setup from the web UI installs the Strata Terminal instead.
    #[serde(default = "default_strata_dir")]
    pub strata_dir: PathBuf,
    /// Strata's install script for the Strata Terminal. Fixed here, never
    /// taken from a request.
    #[serde(default = "default_strata_installer")]
    pub strata_installer: String,
    /// Where the install script puts the Strata Terminal, its engines and
    /// the run configs (its `STRATA_DATA_DIR`).
    #[serde(default = "default_strata_home")]
    pub strata_home: PathBuf,
    /// Where `strata update` keeps the engines and newer Terminal files:
    /// the folder Strata picks itself, which `strata serve` reads too.
    /// Usually left out.
    #[serde(default = "default_strata_components")]
    pub strata_components: PathBuf,
    /// Where the setup puts the model files (60–190 GB each).
    #[serde(default = "default_strata_data_dir")]
    pub strata_data_dir: PathBuf,
    /// Relay mode: the Strata Terminal's Strata listens on this PC only,
    /// and the proxy reaches it through this service's `/relay`. Takes
    /// effect with the next start of Strata.
    #[serde(default)]
    pub relay: bool,
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
fn default_context_start() -> u64 {
    600
}
fn default_stop_settle() -> u64 {
    2000
}
fn default_cancel_wait() -> u64 {
    60
}
fn default_update_stop_wait() -> u64 {
    10
}
fn default_strata_repo() -> String {
    format!("{}.git", crate::releases::REPOSITORY)
}
fn default_strata_installer() -> String {
    "https://github.com/Niko1221/Strata/releases/latest/download/install.ps1".into()
}
/// `%LOCALAPPDATA%\Strata`, the install script's own default.
fn default_strata_home() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from(r"C:\Strata-Terminal"),
        |d| PathBuf::from(d).join("Strata"),
    )
}
/// Strata's own rule (`components_dir` in strata_control/engine_component.py):
/// `STRATA_COMPONENTS_DIR`, else `%LOCALAPPDATA%\Strata\components`.
fn default_strata_components() -> PathBuf {
    if let Some(dir) = std::env::var_os("STRATA_COMPONENTS_DIR") {
        return PathBuf::from(dir);
    }
    default_strata_home().join("components")
}
fn default_strata_dir() -> PathBuf {
    PathBuf::from(r"C:\Strata")
}
fn default_strata_data_dir() -> PathBuf {
    PathBuf::from(r"C:\Strata-data")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Config {} not readable", path.display()))?;
        // Set-Content -Encoding UTF8 in Windows PowerShell writes a BOM.
        let mut cfg: Self = toml::from_str(raw.trim_start_matches('\u{feff}'))
            .with_context(|| format!("Config {} is invalid", path.display()))?;
        cfg.strata_url = cfg.strata_url.trim_end_matches('/').to_string();
        if let Some(mac) = &cfg.mac {
            parse_mac(mac).with_context(|| format!("mac = {mac:?} is not a MAC address"))?;
        }
        if cfg.state_dir.is_none() {
            cfg.state_dir = path.parent().map(Path::to_path_buf);
        }
        Ok(cfg)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.clone().unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn stop_settle(&self) -> Duration {
        Duration::from_millis(self.stop_settle_ms)
    }

    pub fn cancel_wait(&self) -> Duration {
        Duration::from_secs(self.cancel_wait_secs)
    }

    pub fn update_stop_wait(&self) -> Duration {
        Duration::from_secs(self.update_stop_wait_secs)
    }

    pub fn user_idle(&self) -> Duration {
        Duration::from_secs(self.user_idle_minutes * 60)
    }

    /// Where the desktop shortcut reaches the running service.
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.listen.port())
    }
}

/// Parses `AA:BB:CC:DD:EE:FF` or `AA-BB-CC-DD-EE-FF` (the format Windows prints).
pub fn parse_mac(raw: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = raw.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (byte, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.map(|b| format!("{b:02X}")).join(":")
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

    #[test]
    fn start_script_and_programs_may_be_left_out() {
        let dir = std::env::temp_dir().join(format!("strata-pc-agent-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("strata-pc-agent.toml");
        std::fs::write(&path, "token = \"secret\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(cfg.strata_start, None);
        assert!(cfg.strata_processes.is_empty());
    }

    #[test]
    fn configuration_from_the_one_line_setup_parses() {
        // What install.ps1 writes after signing up with the NAS.
        let dir =
            std::env::temp_dir().join(format!("strata-pc-agent-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("strata-pc-agent.toml");
        std::fs::write(
            &path,
            [
                "# PC service config, written by the install command from the NAS.",
                "listen = \"0.0.0.0:8091\"",
                "token = \"0123abcd\"",
                "allowed_clients = [\"192.168.1.10\"]",
                "strata_url = \"http://127.0.0.1:8080\"",
                "nas_url = \"http://192.168.1.10:8090\"",
                "fresh_strata_after_standby = true",
                "strata_api_key = \"strata-secret\"",
            ]
            .join("\r\n")
            // As Windows PowerShell's Set-Content -Encoding UTF8 writes it.
            .replacen("", "\u{feff}", 1),
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(cfg.nas_url.as_deref(), Some("http://192.168.1.10:8090"));
        assert_eq!(
            cfg.allowed_clients,
            ["192.168.1.10".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(cfg.strata_start, None);
    }

    #[test]
    fn parses_mac_addresses() {
        assert_eq!(
            parse_mac("aa-bb-cc-01-02-03"),
            Some([0xaa, 0xbb, 0xcc, 1, 2, 3])
        );
        assert_eq!(
            parse_mac("AA:BB:CC:01:02:03").map(format_mac).as_deref(),
            Some("AA:BB:CC:01:02:03")
        );
        assert_eq!(parse_mac("aa:bb:cc"), None);
        assert_eq!(parse_mac("zz:bb:cc:01:02:03"), None);
    }
}
