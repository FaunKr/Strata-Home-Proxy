//! `GET /status`, `POST /gaming`, `POST /sleep`, `POST /sleep/force`,
//! `POST /strata/kill`, `POST /update`, `GET /strata/key` and the body of
//! every refusal.

use serde::{Deserialize, Serialize};

use crate::StrataJobKind;

/// `GET /status`, and the answer to `POST /gaming`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PcStatus {
    pub gaming_mode: bool,
    /// Strata answers `/health`, or the PC service started it within its
    /// load time and has not stopped it since: a Strata that loads its
    /// model counts as running before it answers.
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
    /// The kind of the running job; missing without one and from older
    /// services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata_job_kind: Option<StrataJobKind>,
    /// The PC service's version, e.g. `0.1.0+558db97`; missing from services
    /// before the update button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The hardware readings; missing from services before them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<Hardware>,
    /// Changes with every new Strata key, never the key itself: the proxy
    /// then fetches it from `GET /strata/key`. Missing from services before
    /// the key per start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata_key_generation: Option<u64>,
}

/// `GET /strata/key`: the Strata key in Strata's model configs, and the
/// generation the status reports for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrataKey {
    pub api_key: String,
    pub generation: u64,
}

/// The hardware readings. A value the PC service could not read is `None`, sent
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

/// The answer to `POST /sleep/force`: what the forced sleep overrode.
/// Standby follows as after an accepted `POST /sleep`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForcedSleep {
    /// Someone used mouse or keyboard within the PC service's limit.
    pub user_active: bool,
    /// Strata was processing a request.
    pub strata_busy: bool,
    /// Gaming mode is on; it stays on after the PC wakes.
    pub gaming_mode: bool,
    /// The kind of the Strata job it cancelled first; downloaded parts stay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_job: Option<StrataJobKind>,
}

/// The answer to `POST /strata/kill`, the forced Strata restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForcedRestart {
    /// The kind of the Strata job it cancelled before the kill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_job: Option<StrataJobKind>,
    /// Strata was started again since the kill. False without a start
    /// script, with `auto_start` off, after a failed start, or when a job
    /// begun meanwhile handles Strata.
    pub started: bool,
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
