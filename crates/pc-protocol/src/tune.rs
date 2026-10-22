//! A model's latest run of Strata's auto-tuner, in `GET /strata`.

use serde::{Deserialize, Serialize};

use crate::{TuneMode, TuneUse};

/// What the tuner found for a model; never applied by the PC service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataTune {
    /// When it ended, as Unix time.
    pub at: u64,
    pub mode: TuneMode,
    #[serde(rename = "use")]
    pub usage: TuneUse,
    /// Strata's outcome: `faster`, `already_fastest`, `busy_pc` (nothing
    /// could be compared), `failed` or `stopped`; a string, so a later one
    /// does not break older proxies.
    pub code: String,
    /// Tokens/s of the model's own settings and of the tuned ones, as the
    /// tuner measured them.
    #[serde(default)]
    pub base_tps: Option<f64>,
    #[serde(default)]
    pub tuned_tps: Option<f64>,
    #[serde(default)]
    pub gain_pct: Option<f64>,
    /// What the tuned settings change, e.g. `--spec 3`, `--kv-grow`,
    /// `STRATA_X=1`, `per request temperature 0.6`.
    #[serde(default)]
    pub changed: Vec<String>,
    /// Strata's sentence on it.
    #[serde(default)]
    pub message: Option<String>,
    /// The preset Strata saved it as, by id and name; `None` when it saved
    /// none.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub preset_name: Option<String>,
}
