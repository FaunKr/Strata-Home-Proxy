//! What the installed Strata offers to tune, in `GET /strata`: the tuner's
//! option table and the engine's own help, read once per Strata version.
//! Values are text as Strata writes them (`0.55`, `auto:32768`).

use serde::{Deserialize, Serialize};

/// Strata's options as the Strata Terminal's own files describe them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StrataOptions {
    /// The Strata Terminal version they were read from.
    pub version: String,
    /// Each engine option the tuner knows; empty when its table could not
    /// be read.
    #[serde(default)]
    pub tuner: Vec<TunerOption>,
    /// Settings the tuner never picks, with Strata's reason.
    #[serde(default)]
    pub never: Vec<NeverRule>,
    /// What the engine prints for `--help`; `None` without a model's engine
    /// that runs.
    #[serde(default)]
    pub engine_help: Option<String>,
}

/// One engine option in the tuner's table (`strata_control/tune_knobs.py`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TunerOption {
    /// The engine flag, e.g. `--spec`.
    pub flag: String,
    /// `false` for a switch that is on by being there.
    pub takes_value: bool,
    /// The range the tuner searches, if it has one.
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
    /// The values the tuner picks from, if it has a list.
    #[serde(default)]
    pub choices: Vec<String>,
    /// How its answers compare with Strata's defaults: `exact` (the same
    /// answers), `placement` (the same model, other numbers on the GPU),
    /// `quality` (can change answers) or `nonrepeatable`.
    pub answer: String,
    /// The values the tuner's own tries set.
    #[serde(default)]
    pub tried: Vec<String>,
}

/// A setting the tuner never picks (`NEVER` in `tune_knobs.py`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NeverRule {
    /// The engine flag, or the environment variable when `env`.
    pub setting: String,
    pub env: bool,
    /// Only these values; all when empty.
    #[serde(default)]
    pub values: Vec<String>,
    /// Strata's name for the test on the value, e.g. `at_least`.
    #[serde(default)]
    pub test: Option<String>,
    /// The bound that test uses.
    #[serde(default)]
    pub limit: Option<String>,
    /// Only on PCs with these traits (`hybrid_cpu`, `gfx12`); all when empty.
    #[serde(default)]
    pub when: Vec<String>,
    /// Only for these model families; all when empty.
    #[serde(default)]
    pub families: Vec<String>,
    pub reason: String,
}
