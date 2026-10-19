//! `GET /status`, `POST /gaming`, `POST /sleep`, `POST /update` and the
//! body of every refusal.

use serde::{Deserialize, Serialize};

/// `GET /status`, and the answer to `POST /gaming`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PcStatus {
    pub gaming_mode: bool,
    #[serde(default)]
    pub strata_running: bool,
    /// Seconds since the last keyboard or mouse input.
    #[serde(default)]
    pub user_idle_secs: u64,
    /// MAC address of the network card the caller reached, so the NAS can
    /// wake the PC without it being typed in; missing when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// A Strata job (setup, download, update, benchmark) runs.
    #[serde(default)]
    pub strata_job: bool,
    /// The PC-Dienst's version, e.g. `0.1.0+558db97`; missing from services
    /// before the update button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The Hardwarewerte; missing from services before them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<Hardware>,
}

/// The Hardwarewerte. A value the PC-Dienst could not read is `None`, sent
/// as `null`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hardware {
    /// The GPU with the most VRAM; `None` if there is none.
    pub gpu: Option<Gpu>,
    pub cpu: Cpu,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub load_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    /// 0 while the fan stands still.
    pub fan_rpm: Option<u32>,
    /// Power draw as a share of the card's power limit.
    pub power_pct: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    pub load_pct: Option<f32>,
    pub ram_used_bytes: Option<u64>,
    pub ram_total_bytes: Option<u64>,
}

/// `POST /gaming`: on ends Strata and blocks sleep, off starts it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamingRequest {
    pub enabled: bool,
}

/// The answer to `POST /sleep` and an accepted `POST /update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub accepted: bool,
    /// Why not, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Answer {
    pub fn accepted() -> Self {
        Self {
            accepted: true,
            reason: None,
        }
    }

    pub fn refused(reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            reason: Some(reason.into()),
        }
    }
}

/// The body of a refusal (400, 409, 500); a refused [`Answer`] reads as one
/// too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Refused {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
        }
    }
}
