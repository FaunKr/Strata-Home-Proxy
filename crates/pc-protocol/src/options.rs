//! What the installed Strata offers to tune, in `GET /strata`: the tuner's
//! option table and the engine's own help, read once per Strata version.
//! Values are text as Strata writes them (`0.55`, `auto:32768`).

use std::collections::BTreeMap;

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
    /// The tries the tuner makes beyond its ranges.
    #[serde(default)]
    pub tries: Vec<TunerTry>,
    /// What the engine prints for `--help`; `None` without a model's engine
    /// that runs.
    #[serde(default)]
    pub engine_help: Option<String>,
    /// Strata's speed switches (`strata_control/speed_switches.py`);
    /// empty before Strata 0.2.7, or from older services.
    #[serde(default)]
    pub switches: Vec<SpeedSwitch>,
    /// Engine environment variables Strata documents but lists in no
    /// table, those that apply to this PC; empty from older services.
    #[serde(default)]
    pub documented: Vec<DocumentedVariable>,
}

/// One of Strata's speed switches, as its app and Terminal offer it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeedSwitch {
    /// Strata's words for it, e.g. `Faster long prompts`.
    pub label: String,
    /// Strata's sentence on what it does and what the other state does.
    pub effect: String,
    /// Whether the feature is on while its variables are absent.
    pub default_on: bool,
    /// The variables that take it out of its default state.
    pub env: BTreeMap<String, String>,
    /// The installed models it applies to on this PC, by id.
    #[serde(default)]
    pub models: Vec<String>,
}

/// An engine environment variable from Strata's documentation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentedVariable {
    pub name: String,
    /// The value to set; `None` when the value is for the AI to choose.
    #[serde(default)]
    pub value: Option<String>,
    /// What it does and what it costs, from Strata's documentation.
    pub text: String,
    /// Whether it changes answers (at rounding level).
    pub changes_answers: bool,
    /// Only for a model whose engine options hold one of these; all when
    /// empty.
    #[serde(default)]
    pub needs_args: Vec<String>,
    /// Not for a model whose engine options hold one of these.
    #[serde(default)]
    pub not_with_args: Vec<String>,
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
}

/// One of the tuner's own tries (`ARMS` in `tune_knobs.py`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TunerTry {
    /// Strata's words for it.
    pub label: String,
    /// As in [`TunerOption::answer`].
    pub answer: String,
    /// The engine arguments it sets, e.g. `["--spec", "3", "--kv-grow"]`;
    /// empty for a try that sets something else.
    #[serde(default)]
    pub args: Vec<String>,
    /// The engine environment variables it sets, `None` for one it
    /// removes; empty from an older PC service too.
    #[serde(default)]
    pub env: BTreeMap<String, Option<String>>,
    /// Only for these model families; all when empty.
    #[serde(default)]
    pub families: Vec<String>,
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
