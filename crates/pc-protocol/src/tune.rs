//! A model's latest run of Strata's auto-tuner and its preset, in
//! `GET /strata`.

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

/// One measurement of a running tune, as Strata's status file has it:
/// `base` is the model's own settings, any other label the setting tried.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TuneSpeed {
    pub label: String,
    /// Tokens per second, as the tuner judges them for its use.
    pub tok_s: f64,
}

/// A model's presets, from `strata presets status` for its run config: they
/// belong to the model file and its choices (context among them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataPresets {
    /// The active preset, else the newest; `None` when there is none.
    #[serde(default)]
    pub latest: Option<StrataPreset>,
    /// The model's last tune saved a preset for other choices: nothing
    /// is tuned for its context now.
    #[serde(default)]
    pub other_context: bool,
}

/// One of Strata's saved tunings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataPreset {
    pub id: String,
    pub name: String,
    /// Strata starts the model with it.
    pub active: bool,
    /// For the active one: whether it still fits this PC, engine and model
    /// file.
    #[serde(default)]
    pub state: Option<PresetState>,
    /// Strata's plain words on what changed, e.g. "the graphics driver
    /// changed".
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub gain_pct: Option<f64>,
    /// What it sets, written as [`StrataTune::changed`].
    #[serde(default)]
    pub settings: Vec<String>,
    /// Answer tokens/s the tuner measured with it and with the model's own
    /// settings.
    #[serde(default)]
    pub decode_tps: Option<f64>,
    #[serde(default)]
    pub base_tps: Option<f64>,
}

/// Strata's `staleness` (`strata_control/presets.py`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetState {
    Fresh,
    /// Still applied; a re-check is offered (new driver, engine update).
    Soft,
    /// Not applied (new graphics card, new model file); a new tune is
    /// offered.
    Hard,
}
