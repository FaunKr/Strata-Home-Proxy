//! `GET /strata`: Strata on the PC, its models, Strata's catalog,
//! benchmarks and what an update brings.

use serde::{Deserialize, Serialize};

use crate::{MachineFacts, StrataJob, StrataOptions};

/// The longest context `set_context` takes: the model's trained length,
/// which needs no rope scaling. Longer ones only Setup sets up.
pub const MAX_SWITCHED_CONTEXT: u32 = 262_144;

/// `GET /strata` as the PC service answers it: what the Strata page shows,
/// and the incoming commits, which stay on the NAS.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataReport {
    #[serde(flatten)]
    pub info: StrataInfo,
    /// What newer commits bring, while there are some; missing before the
    /// first check and from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<Incoming>,
}

impl StrataReport {
    pub fn into_parts(self) -> (StrataInfo, Option<Incoming>) {
        (self.info, self.update)
    }
}

/// Strata on the PC. Never holds the Strata key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataInfo {
    pub installed: bool,
    /// How Strata is installed; `None` before it is and from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<StrataInstall>,
    /// An old Strata folder from source is still there: the one Strata
    /// runs from, or the one left after the switch to the Strata Terminal;
    /// `false` from older services.
    #[serde(default)]
    pub old_strata_folder: bool,
    /// The Strata Terminal's Strata has answered `/health` once; `false`
    /// before and from older services.
    #[serde(default)]
    pub terminal_served: bool,
    /// The Strata Terminal's version, or the short commit of a Strata
    /// folder from source.
    #[serde(default)]
    pub version: Option<String>,
    /// Whether Strata's source has newer commits; `None` until known.
    #[serde(default)]
    pub update_available: Option<bool>,
    pub models: Vec<StrataModel>,
    pub families: Vec<StrataFamily>,
    pub catalog: Vec<StrataSize>,
    #[serde(default)]
    pub recommended: Option<String>,
    pub contexts: Vec<u32>,
    pub default_context: u32,
    pub ram_gb: f64,
    /// Dedicated VRAM of the biggest card, which Gyro's fit and the
    /// recommendation count; `None` without a card or from older services.
    #[serde(default)]
    pub vram_gb: Option<f64>,
    #[serde(default)]
    pub disk_free_gb: Option<f64>,
    pub data_dir: String,
    /// What the PC is; missing from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineFacts>,
    /// The last successful run, for proxies that know only one.
    #[serde(default)]
    pub benchmark: Option<StrataBenchmark>,
    /// Each model's last run; empty from older services.
    #[serde(default)]
    pub benchmarks: Vec<StrataBenchmark>,
    #[serde(default)]
    pub job: Option<StrataJob>,
    /// What the installed Strata Terminal offers to tune; `None` before it
    /// was read, without a Terminal and from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<StrataOptions>,
}

/// How Strata is installed on the PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrataInstall {
    /// The Strata Terminal, from Strata's install script.
    Terminal,
    /// An older Strata folder from source (a Git clone).
    Source,
}

/// An installed model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataModel {
    /// Setup's id, e.g. `IQ3_XXS` or `coder-IQ1_M`.
    pub id: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    pub disk_bytes: u64,
    pub active: bool,
    /// Its config loads a control vector, Strata's or another one; `None`
    /// from older services.
    #[serde(default)]
    pub speed_projection: Option<bool>,
    /// The context it starts with (`--max-context`); `None` if its config
    /// does not say, or from older services.
    #[serde(default)]
    pub context: Option<u32>,
    /// Strata's recovery from repeated reasoning passages: `off`, `stop` or
    /// `recover`; `None` from older services.
    #[serde(default)]
    pub loop_recovery: Option<String>,
    /// What a request without sampling fields gets; `None` while Strata's
    /// version cannot be read, or from older services.
    #[serde(default)]
    pub sampling: Option<StrataSampling>,
    /// Its run config as JSON text, laid out as Strata writes it, without
    /// `api_key` and with every path cut to its file name; `None` if it
    /// cannot be read, or from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Its Chat settings from Strata's web app as JSON text, cut the same
    /// way; `None` without the file, or from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_settings: Option<String>,
    /// The output of its latest load; `None` before one was seen, or from
    /// older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_lines: Option<crate::LoadLines>,
}

/// Which defaults the PC's Strata applies to a request without sampling
/// fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingSupport {
    /// The model card's sets, then the config and the Chat settings.
    ModelCard,
    /// Only the config's block and the Chat settings.
    Config,
    /// None: Strata decodes greedily.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingSource {
    ModelCard,
    Config,
    ChatSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplingField {
    pub name: String,
    pub value: f64,
    pub source: SamplingSource,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataSampling {
    pub support: SamplingSupport,
    /// With thinking; empty is greedy.
    pub thinking: Vec<SamplingField>,
    /// Without thinking.
    pub instruct: Vec<SamplingField>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataFamily {
    pub id: String,
    pub title: String,
    /// Setup offers the experimental speed projection; older services do
    /// not say so.
    #[serde(default)]
    pub speed_projection: bool,
}

/// A size from Strata's catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataSize {
    /// Model id, e.g. `coder-IQ1_M`.
    pub id: String,
    pub family: String,
    pub size: String,
    pub download_gb: f64,
    pub ram_gb: u32,
    /// `ram_gb` is RAM and VRAM together (Gyro); `false` from older services.
    #[serde(default)]
    pub memory_with_vram: bool,
    /// The PC has the memory for it.
    pub fits: bool,
    /// Parts are downloaded already; installing continues there.
    pub partly_downloaded: bool,
}

/// A model's last benchmark run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataBenchmark {
    pub model: String,
    /// Tokens per second while answering, e.g. `38.5 Token/s`; empty when
    /// the run failed.
    pub result: String,
    /// Unix seconds.
    pub at: u64,
    /// Local time text for the web UI, filled in by the proxy; the
    /// PC service never sends it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at_text: String,
    /// Chat decode tokens per second; missing from older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_tps: Option<f64>,
    /// Tokens per second reading a 4K prompt, if the benchmark said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tps: Option<f64>,
    /// What it was measured with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_projection: Option<bool>,
    /// Short commit of the Strata folder, or the Strata Terminal's
    /// version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why its run in a comparison gave no result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What measured it; missing from older records, which came from
    /// `BENCH.bat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<BenchTool>,
}

/// The benchmark that made a result. The two measure differently, so their
/// tokens/s are not comparable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchTool {
    /// `BENCH.bat` of a Strata folder from source.
    BenchBat,
    /// Strata's standard benchmark, `strata bench run`.
    StrataBench,
}

/// What an update would bring: the commits Strata's Git source has and the
/// Strata folder lacks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incoming {
    /// Full hash of the upstream commit.
    pub upstream: String,
    /// All incoming commits; `commits` holds at most 200 of them.
    pub total: u64,
    /// Newest first.
    pub commits: Vec<IncomingCommit>,
    pub files: Vec<ChangedFile>,
    /// URL of the upstream remote.
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingCommit {
    pub hash: String,
    /// Commit time, Unix seconds.
    pub time: u64,
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

/// A file the incoming commits change; binary files count 0 lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}
