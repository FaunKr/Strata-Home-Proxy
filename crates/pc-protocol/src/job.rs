//! Strata jobs: `POST /strata/jobs`, `GET /strata/job` and
//! `POST /strata/job/cancel`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrataJobKind {
    Install,
    AddModel,
    Activate,
    Delete,
    Update,
    Benchmark,
    SetKey,
    SetSpeedProjection,
    SetContext,
    CompareModels,
}

impl StrataJobKind {
    pub const ALL: [Self; 10] = [
        Self::Install,
        Self::AddModel,
        Self::Activate,
        Self::Delete,
        Self::Update,
        Self::Benchmark,
        Self::SetKey,
        Self::SetSpeedProjection,
        Self::SetContext,
        Self::CompareModels,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrataJobState {
    Running,
    Done,
    Failed,
    Cancelled,
    /// The PC or its service restarted while it ran.
    Interrupted,
}

/// A job on the PC. Never holds a key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataJob {
    /// Counts up per job on the PC; missing from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub kind: StrataJobKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: StrataJobState,
    /// Setup's step, e.g. "4/7: Downloading the model".
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub done_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub bytes_per_sec: Option<u64>,
    #[serde(default)]
    pub eta_secs: Option<u64>,
    /// The last lines of output.
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// What came out, e.g. a benchmark's tokens per second.
    #[serde(default)]
    pub result: Option<String>,
    /// Unix seconds; 0 from older services.
    #[serde(default)]
    pub started: u64,
    #[serde(default)]
    pub ended: Option<u64>,
}

/// `POST /strata/jobs`. The PC service refuses fields it does not know, so an
/// optional field stays out when unset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StrataJobRequest {
    Install {
        family: String,
        size: String,
        context: u32,
        api_key: String,
        /// Strata's experimental speed projection; only for families whose
        /// catalog entry offers it.
        #[serde(default, skip_serializing_if = "is_false")]
        experimental_speed_projection: bool,
    },
    AddModel {
        family: String,
        size: String,
        context: u32,
        /// The key the proxy uses; without it the PC service's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        experimental_speed_projection: bool,
    },
    Activate {
        model: String,
    },
    Delete {
        model: String,
    },
    Update,
    Benchmark,
    SetKey {
        api_key: String,
    },
    SetSpeedProjection {
        model: String,
        on: bool,
    },
    SetContext {
        model: String,
        context: u32,
    },
    CompareModels,
}

impl StrataJobRequest {
    pub fn kind(&self) -> StrataJobKind {
        match self {
            Self::Install { .. } => StrataJobKind::Install,
            Self::AddModel { .. } => StrataJobKind::AddModel,
            Self::Activate { .. } => StrataJobKind::Activate,
            Self::Delete { .. } => StrataJobKind::Delete,
            Self::Update => StrataJobKind::Update,
            Self::Benchmark => StrataJobKind::Benchmark,
            Self::SetKey { .. } => StrataJobKind::SetKey,
            Self::SetSpeedProjection { .. } => StrataJobKind::SetSpeedProjection,
            Self::SetContext { .. } => StrataJobKind::SetContext,
            Self::CompareModels => StrataJobKind::CompareModels,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !b
}
