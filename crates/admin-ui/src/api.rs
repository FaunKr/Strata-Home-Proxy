//! Server functions: the HTTP interface between the pages and the proxy, all
//! under `/admin/api/`.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};
// What the PC service reports, shown as it comes (docs/adr/0002).
pub use strata_pc_protocol::{
    Cpu, ForcedRestart, Gpu, Hardware, SamplingField, SamplingSource, SamplingSupport,
    StrataBenchmark, StrataFamily, StrataInfo, StrataJob, StrataJobKind, StrataJobState,
    StrataModel, StrataSampling, StrataSize, StrataTune, TuneMode, TuneUse,
};

#[cfg(feature = "ssr")]
use crate::server::{
    backend, clear_cookie, client_ip, request_host, require_login, set_cookie, token,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Session {
    Anonymous,
    /// Logged in with the default password, which must be changed first.
    MustChange,
    Ready,
}

#[server(prefix = "/admin/api", endpoint = "session")]
pub async fn get_session() -> Result<Session, ServerFnError> {
    Ok(backend()?.session(token().await.as_deref()))
}

#[server(prefix = "/admin/api", endpoint = "login")]
pub async fn login(password: String) -> Result<(), ServerFnError> {
    let token = backend()?
        .login(&password, client_ip().await)
        .map_err(|e| {
            crate::server::set_status(axum::http::StatusCode::UNAUTHORIZED);
            ServerFnError::new(e)
        })?;
    set_cookie(&token);
    Ok(())
}

#[server(prefix = "/admin/api", endpoint = "logout")]
pub async fn logout() -> Result<(), ServerFnError> {
    if let Some(token) = token().await {
        backend()?.logout(&token);
    }
    clear_cookie();
    Ok(())
}

#[server(prefix = "/admin/api", endpoint = "change-password")]
pub async fn change_password(
    current: String,
    new: String,
    repeat: String,
) -> Result<(), ServerFnError> {
    let token = require_login(true).await?;
    if new != repeat {
        return Err(ServerFnError::new("The two new passwords do not match."));
    }
    let token = backend()?
        .change_password(&token, &current, &new, client_ip().await)
        .map_err(ServerFnError::new)?;
    set_cookie(&token);
    Ok(())
}

/// Where a setting's value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Reported by the PC service (MAC only).
    Reported,
    /// Saved on this page.
    Saved,
    Env,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEntry {
    /// Environment variable name, e.g. `STRATA_URL`.
    pub key: String,
    /// The value in effect; for secrets only "set".
    pub value: Option<String>,
    pub source: Source,
    /// The value saved on this page, if any (never for secrets).
    pub saved: Option<String>,
    /// Takes effect only after a restart.
    pub restart: bool,
    pub secret: bool,
    pub optional: bool,
    /// What an empty field uses, for limits the proxy suggests itself.
    pub suggestion: Option<SettingSuggestion>,
}

/// A value the proxy works out itself while none is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingSuggestion {
    pub secs: u64,
    /// What it comes from, e.g. "slowest model IQ3_XXS at 12 tok/s".
    pub from: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// False without `DATA_DIR`: changes are lost on restart.
    pub persistent: bool,
    /// False while no Strata URL is set.
    pub configured: bool,
    pub entries: Vec<SettingEntry>,
    /// The proxy's own version.
    pub version: String,
    /// `DATA_DIR` as the proxy uses it.
    pub data_dir: Option<String>,
}

#[server(prefix = "/admin/api", endpoint = "settings")]
pub async fn get_settings() -> Result<SettingsView, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.settings())
}

/// One value of a settings section, as the page sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingValue {
    pub key: String,
    pub value: String,
}

/// Saves the changed values of a section together: all or none. An empty
/// secret stays as it is. An invalid value's error reads "KEY: message".
#[server(prefix = "/admin/api", endpoint = "save-settings", input = server_fn::codec::Json)]
pub async fn save_settings(values: Vec<SettingValue>) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?
        .save_settings(&values)
        .map_err(|(key, message)| match key {
            Some(key) => bad_request(format!("{key}: {message}")),
            None => bad_request(message),
        })
}

/// Splits a save error into the key it names and its message.
pub fn setting_error(text: &str) -> (Option<&str>, &str) {
    match text.split_once(": ") {
        Some((key, message))
            if !key.is_empty() && key.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') =>
        {
            (Some(key), message)
        }
        _ => (None, text),
    }
}

#[server(prefix = "/admin/api", endpoint = "reset-setting")]
pub async fn reset_setting(key: String) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.reset_setting(&key).map_err(bad_request)
}

/// A client's API key, without the secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub name: String,
    /// The start of the key, to match it with a client's configuration.
    pub prefix: String,
    /// Unix time in seconds.
    pub created: u64,
    /// Unix time in seconds of the last request with this key.
    pub last_used: Option<u64>,
    /// `created` as the NAS shows times, e.g. `17.10. 14:03`.
    pub created_text: String,
    pub last_used_text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeysView {
    /// False without `DATA_DIR`: keys are lost on restart.
    pub persistent: bool,
    pub keys: Vec<ApiKeyInfo>,
}

/// A new key; the only time it is shown in full.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedKey {
    pub key: String,
    pub info: ApiKeyInfo,
}

#[server(prefix = "/admin/api", endpoint = "api-keys")]
pub async fn get_api_keys() -> Result<ApiKeysView, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.api_keys())
}

#[server(prefix = "/admin/api", endpoint = "create-api-key")]
pub async fn create_api_key(name: String) -> Result<CreatedKey, ServerFnError> {
    require_login(false).await?;
    backend()?.create_api_key(&name).map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "revoke-api-key")]
pub async fn revoke_api_key(name: String, prefix: String) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?
        .revoke_api_key(&name, &prefix)
        .map_err(bad_request)
}

/// The one-line command that sets up the PC service, with its one-time code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PcSetupCommand {
    pub command: String,
    pub code: String,
    /// How long the code is valid.
    pub valid_minutes: u64,
    /// Removes the PC service again; needs no code.
    pub uninstall_command: String,
}

/// A new one-time code; the previous one stops working.
#[server(prefix = "/admin/api", endpoint = "pc-setup-command")]
pub async fn new_pc_setup_command() -> Result<PcSetupCommand, ServerFnError> {
    require_login(false).await?;
    let host = request_host().await;
    backend()?
        .pc_setup_command(host.as_deref().unwrap_or_default())
        .map_err(bad_request)
}

#[cfg(feature = "ssr")]
fn bad_request(message: String) -> ServerFnError {
    crate::server::set_status(axum::http::StatusCode::BAD_REQUEST);
    ServerFnError::new(message)
}

/// The message of a server function error, without the framework's prefix.
pub fn error_text(err: &ServerFnError) -> String {
    match err {
        ServerFnError::ServerError(msg) => msg.clone(),
        other => other.to_string(),
    }
}

/// How far the proxy is set up; the overview's checklist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    /// False while no Strata URL is set.
    pub configured: bool,
    /// False while no MAC address is known: the PC cannot be woken.
    pub mac_known: bool,
    /// False without a PC service: no gaming mode and no automatic sleep.
    pub agent_configured: bool,
    /// False while no API key exists: every client gets 401.
    pub api_key_exists: bool,
    /// False without `DATA_DIR`: settings and keys are lost on a restart.
    pub persistent: bool,
}

impl Setup {
    /// Every item of the checklist is done.
    pub fn complete(&self) -> bool {
        self.configured
            && self.mac_known
            && self.agent_configured
            && self.api_key_exists
            && self.persistent
    }
}

/// How far the proxy is set up, without asking the PC; the overview's
/// checklist before its live stream arrives.
#[server(prefix = "/admin/api", endpoint = "setup")]
pub async fn get_setup() -> Result<Setup, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.setup())
}

/// What the overview shows; sent as a whole on every change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    /// The proxy's clock (Unix seconds), so the page can count down without
    /// trusting the viewer's clock.
    pub now: u64,
    #[serde(flatten)]
    pub setup: Setup,
    /// After this long without requests the PC is put to sleep.
    pub idle_timeout_secs: u64,
    pub pc: PcView,
    pub strata: StrataView,
    /// Start or end of the last API request, as local time text.
    pub last_request: Option<String>,
    pub sleep: SleepView,
    /// End of "keep awake", as local time text.
    pub keep_awake_until: Option<String>,
    /// The latest PC events, newest first.
    pub events: Vec<EventView>,
    /// A Strata job running on the PC, with its progress.
    pub strata_job: Option<StrataJob>,
    /// The PC service's version and updates; `None` until it answered once
    /// since the proxy started.
    pub pc_service: Option<PcServiceView>,
    pub hardware: HardwareView,
    pub power: PowerView,
    /// The history of the last 7 days, summed at most once a minute.
    pub week: HistoryView,
}

/// The power of the power plug for the overview; independent of the PC's
/// state, since the plug also answers while the PC sleeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PowerView {
    /// No power plug is set up (`SHELLY_URL`).
    NotConfigured,
    /// The power plug did not answer usefully.
    Unreachable,
    /// The current power in watts.
    Watts(f64),
}

/// The hardware readings for the overview.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HardwareView {
    /// No values: the PC is not awake, or its service does not answer.
    Unavailable,
    /// The PC service answers but is too old to report them.
    Unsupported,
    Values(Hardware),
}

/// The PC service's version against the one the NAS carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PcServiceView {
    /// The version it reported last, also while the PC sleeps; `None` for a
    /// service too old to report one.
    pub version: Option<String>,
    /// The version of the program in the image; `None` if it carries none.
    pub available: Option<String>,
    /// The image carries another version than the PC runs.
    pub update_available: bool,
    /// An update was accepted and the new version has not reported yet.
    pub updating: bool,
    /// Why the last update failed.
    pub update_error: Option<String>,
    /// The MAC address the service reported, e.g. `3c:7c:3f:12:a4:9e`.
    pub reported_mac: Option<String>,
}

/// One message of the Strata output stream (`/admin/api/strata-output-stream`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputMessage {
    /// The kept lines come again from here on; the page drops what it has.
    Reset,
    Line {
        text: String,
    },
    /// Takes the place of the last line, as a progress bar does in a console.
    Replace {
        text: String,
    },
    Notice {
        notice: OutputNotice,
    },
}

/// Why the Strata output has stopped, or `Running` when it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputNotice {
    Running,
    Asleep,
    NoAgent,
    /// The PC service is too old for the Strata output.
    TooOld,
    /// The PC service refuses the proxy: token or allowed addresses.
    Refused,
    Gaming,
    NotRunning,
    NotStartedByAgent,
}

/// Kinds of events in the history; their snake_case names are what the
/// events database stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WakeSent,
    Awake,
    WakeTimeout,
    SleepAccepted,
    SleepRefused,
    GamingRejected,
    // What the proxy noticed about the PC without causing it; recorded only on
    // a change, never on every check.
    PcSeenAwake,
    PcSeenAsleep,
    PcSeenGaming,
    /// The proxy started; time it could not observe counts as unknown.
    ProxyStarted,
    /// Recorded at the next start, with the time the proxy was last seen
    /// running; from then on the history is unknown.
    ProxyStopped,
    PasswordChanged,
    /// Detail: the setting's name, never its value.
    SettingSaved,
    SettingReset,
    /// The PC service reported a new MAC address (detail: the address).
    MacLearned,
    /// Detail: the key's name, never the key.
    ApiKeyCreated,
    ApiKeyRevoked,
    // Buttons on the overview. Wake and sleep are recorded when pressed, their
    // outcome as above; the others once they took effect.
    WakeRequested,
    SleepRequested,
    GamingOn,
    GamingOff,
    /// Detail: until when.
    KeepAwakeSet,
    KeepAwakeEnded,
    /// Strata jobs on the PC; detail: kind and model, and why it failed.
    StrataJobStarted,
    StrataJobDone,
    StrataJobFailed,
    StrataJobCancelled,
    /// The PC service started Strata with a new key and the proxy took it
    /// over.
    StrataKeyChanged,
    /// A PC signed up with a one-time code (detail: the PC service's address).
    PcServiceRegistered,
    /// The update button (detail: who); then the new version or why it failed.
    PcServiceUpdateRequested,
    PcServiceUpdated,
    PcServiceUpdateFailed,
    /// A forced sleep the PC service took; detail: who, what it overrode
    /// and the job it cancelled. `SleepAccepted` follows.
    ForcedSleep,
    /// A forced Strata restart; detail: who, the job it cancelled and
    /// whether Strata started again.
    ForcedStrataRestart,
    /// "Generate summary now" on the Version tab (detail: who).
    UpdateSummaryRequested,
}

impl EventKind {
    /// About the web UI itself rather than the PC; left out of the overview.
    pub fn is_admin(self) -> bool {
        matches!(
            self,
            Self::PasswordChanged
                | Self::SettingSaved
                | Self::SettingReset
                | Self::ApiKeyCreated
                | Self::ApiKeyRevoked
        )
    }

    /// Marks the proxy sets for the history on its own; left out of the
    /// overview, where the events that caused them already show.
    pub fn is_history_mark(self) -> bool {
        matches!(
            self,
            Self::PcSeenAwake
                | Self::PcSeenAsleep
                | Self::PcSeenGaming
                | Self::ProxyStarted
                | Self::ProxyStopped
        )
    }
}

/// How long the "keep awake" buttons keep the PC awake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeepAwakeChoice {
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "4h")]
    FourHours,
    #[serde(rename = "8h")]
    EightHours,
    /// Until 8 o'clock the next morning, in the NAS's time zone.
    #[serde(rename = "morning")]
    Morning,
    /// Ends keeping awake.
    #[serde(rename = "off")]
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PcView {
    Unknown,
    Asleep,
    Waking,
    Awake,
    Gaming,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StrataView {
    Offline,
    /// The process runs but does not answer yet: starting, or restarted by
    /// the watchdog.
    Loading,
    /// Answers, but the model is not in memory; loaded with the next request.
    Unloaded,
    Ready,
    Busy {
        processing: u32,
        /// Waiting requests; only known from Strata's metrics.
        deferred: Option<u32>,
        tokens_per_sec: Option<f64>,
    },
}

/// When the PC goes to sleep, or why it does not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SleepView {
    /// Asked to sleep at this time (Unix seconds) unless a request comes.
    At(u64),
    Asleep,
    Waking,
    Gaming,
    KeptAwake {
        until: String,
    },
    Busy,
    /// The PC service refused; asked again at `retry_at` (Unix seconds).
    Refused {
        reason: String,
        retry_at: u64,
    },
    /// No PC service set up, so the proxy never asks.
    NoAgent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventView {
    /// Local time text.
    pub time: String,
    pub kind: EventKind,
    pub detail: Option<String>,
}

#[server(prefix = "/admin/api", endpoint = "wake")]
pub async fn wake() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.wake().map_err(bad_request)
}

/// What the PC service answered to Sleep now; mirrors the proxy's
/// `agent::SleepAnswer`. A refusal is an answer, not an error, so the
/// Overview can offer Force sleep under its reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SleepAnswer {
    Accepted,
    Refused { reason: String },
}

#[server(prefix = "/admin/api", endpoint = "sleep-now")]
pub async fn sleep_now() -> Result<SleepAnswer, ServerFnError> {
    require_login(false).await?;
    backend()?.sleep_now().await.map_err(bad_request)
}

/// A forced sleep, after a refused one or in gaming mode: only an update
/// of the PC service refuses it. Never wakes the PC.
#[server(prefix = "/admin/api", endpoint = "force-sleep")]
pub async fn force_sleep() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.force_sleep().await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "gaming")]
pub async fn set_gaming(enabled: bool) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.set_gaming(enabled).await.map_err(bad_request)
}

/// Updates the PC service to the program the NAS carries, waking a sleeping
/// PC first.
#[server(prefix = "/admin/api", endpoint = "pc-service-update")]
pub async fn update_pc_service() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.update_pc_service().await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "keep-awake")]
pub async fn keep_awake(choice: KeepAwakeChoice) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.keep_awake(choice).map_err(bad_request)
}

// --- Strata on the PC: setup, models, update, benchmark ---

/// The Strata page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataPage {
    /// Why the PC service does not answer now, e.g. the PC sleeps.
    pub problem: Option<String>,
    pub gaming: bool,
    /// What the PC service reported, now or the last time it answered.
    pub info: Option<StrataInfo>,
    /// `info` is from an earlier answer.
    pub stale: bool,
    /// The local time of that earlier answer, while `info` is from one.
    #[serde(default)]
    pub as_of: Option<String>,
    /// What an available update brings; `None` without one, or when the PC
    /// service does not report it.
    #[serde(default)]
    pub update: Option<StrataUpdate>,
    /// The PC service reports no Strata key: it is older than the key per
    /// start of Strata.
    #[serde(default)]
    pub key_unreported: bool,
}

/// What a Strata update brings since the installed version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataUpdate {
    pub summary: UpdateSummary,
    /// Newest first; empty while the PC sleeps after a restart of the proxy.
    pub commits: Vec<UpdateCommit>,
    /// Incoming commits not in `commits`.
    pub left_out: u64,
    /// GitHub's comparison of the installed and the newest version.
    pub compare_url: Option<String>,
    /// Why "Generate summary now" cannot start now; `None` if it can.
    #[serde(default)]
    pub blocked: Option<String>,
    /// The PC sleeps: "Generate summary now" wakes it.
    #[serde(default)]
    pub wakes_pc: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateCommit {
    pub short: String,
    pub subject: String,
    pub date_text: String,
    pub url: Option<String>,
}

/// The summary of an update; without Strata's, one from the numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateSummary {
    /// Written by Strata with `model`.
    Written { lines: Vec<String>, model: String },
    /// Strata is writing it now.
    Writing { fallback: Vec<String> },
    Unavailable {
        fallback: Vec<String>,
        /// Why the last attempt for these commits failed.
        #[serde(default)]
        reason: Option<String>,
    },
}

#[server(prefix = "/admin/api", endpoint = "strata")]
pub async fn get_strata() -> Result<StrataPage, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.strata().await)
}

pub use strata_job::*;

// Each argument of the server function is a form field of its request, which
// clippy counts against it in the code the macro writes too.
#[allow(clippy::too_many_arguments)]
mod strata_job {
    use super::*;

    /// Starts a job on the PC, waking it first if it sleeps.
    #[server(prefix = "/admin/api", endpoint = "strata-job")]
    pub async fn start_strata_job(
        kind: StrataJobKind,
        family: Option<String>,
        size: Option<String>,
        context: Option<u32>,
        model: Option<String>,
        speed_projection: Option<bool>,
        tune_mode: Option<TuneMode>,
        tune_use: Option<TuneUse>,
    ) -> Result<(), ServerFnError> {
        require_login(false).await?;
        let form = StrataJobForm {
            kind,
            family,
            size,
            context,
            model,
            speed_projection: speed_projection.unwrap_or(false),
            tune_mode: tune_mode.unwrap_or_default(),
            tune_use: tune_use.unwrap_or_default(),
        };
        backend()?.start_strata_job(form).await.map_err(bad_request)
    }
}

/// A forced Strata restart: the PC service cancels a running job, kills
/// Strata and starts it again where it can. Never wakes the PC.
#[server(prefix = "/admin/api", endpoint = "strata-force-restart")]
pub async fn force_restart_strata() -> Result<ForcedRestart, ServerFnError> {
    require_login(false).await?;
    backend()?.force_restart_strata().await.map_err(bad_request)
}

/// Has Strata write the update summary now, waking the PC first if it
/// sleeps (#193).
#[server(prefix = "/admin/api", endpoint = "strata-update-summary")]
pub async fn generate_update_summary() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?
        .generate_update_summary()
        .await
        .map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "strata-cancel")]
pub async fn cancel_strata_job() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.cancel_strata_job().await.map_err(bad_request)
}

/// The optimization prompt for an installed model; never wakes the PC.
#[server(prefix = "/admin/api", endpoint = "strata-optimization-prompt")]
pub async fn get_optimization_prompt(model: String) -> Result<String, ServerFnError> {
    require_login(false).await?;
    backend()?
        .optimization_prompt(model)
        .await
        .map_err(bad_request)
}

// --- The code benchmark, run by the container code-bench on the NAS ---

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeBenchState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// One task of a run, as the runner reports it (tools/code-bench/runner.py).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBenchTask {
    pub name: String,
    pub title: String,
    /// `pending`, `running`, `done` or `cancelled`.
    pub state: String,
    #[serde(default)]
    pub total_secs: Option<f64>,
    /// Model calls opencode made.
    #[serde(default)]
    pub steps: u32,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Output tokens over the time the model calls took.
    #[serde(default)]
    pub output_tps: Option<f64>,
    /// Hidden tests passed, out of `total`.
    #[serde(default)]
    pub passed: u32,
    #[serde(default)]
    pub total: u32,
    /// opencode was stopped at the time limit.
    #[serde(default)]
    pub timed_out: bool,
    /// The Python files the agent left, by path (a `files` task).
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub test_output: String,
    #[serde(default)]
    pub error: Option<String>,
    /// `files` (its files and hidden tests sent along) or `repo` (a
    /// checkout in the runner's image, scored by the feature's tests).
    #[serde(default = "files_kind")]
    pub kind: String,
    /// A `repo` task: the agent's change against the checkout.
    #[serde(default)]
    pub diff: Option<String>,
    /// A `repo` task: tests that passed on the checkout and fail after the
    /// change, as `<test binary>::<test>`.
    #[serde(default)]
    pub broken: Vec<String>,
}

fn files_kind() -> String {
    "files".into()
}

/// A run of the tasks with one model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBenchRun {
    pub model: String,
    pub state: CodeBenchState,
    #[serde(default)]
    pub error: Option<String>,
    /// Unix seconds.
    pub started: u64,
    #[serde(default)]
    pub ended: Option<u64>,
    pub tasks: Vec<CodeBenchTask>,
    /// Local time of the end, filled in by the proxy.
    #[serde(default)]
    pub at_text: String,
    /// A later run of this model that failed (to load, or in the runner),
    /// noted on the result it left standing: why, and when (Unix seconds).
    #[serde(default)]
    pub failed_later: Option<String>,
    #[serde(default)]
    pub failed_at: Option<u64>,
    /// Local time of `failed_at`, filled in by the proxy.
    #[serde(default)]
    pub failed_at_text: String,
}

/// A task of the code benchmark, for the card: what a run holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBenchTaskInfo {
    pub name: String,
    pub title: String,
    /// Its time limit in seconds.
    pub limit_secs: u64,
}

/// The progress of a run that benchmarks all models: `Model {model} of
/// {models} · task {task} of {tasks}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBenchAll {
    /// Models in the run (m), and the one being benchmarked now (n), 1-based.
    pub models: usize,
    pub model: usize,
    /// Tasks per model (N) and the one being solved now (k), 1-based.
    pub tasks: usize,
    pub task: usize,
    /// The model active before the run, activated again at the end.
    pub restore: String,
}

/// The code benchmark card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBenchPage {
    /// Why no run can start now, e.g. the container does not answer.
    pub problem: Option<String>,
    /// The running run, or the last one if it did not finish.
    pub run: Option<CodeBenchRun>,
    /// Each model's last finished run. A model that failed without one gets
    /// its error row; one with a result keeps it, the failure noted on it.
    pub results: Vec<CodeBenchRun>,
    /// A run over all models, or the last one that did not finish.
    pub all: Option<CodeBenchAll>,
    /// How many models are installed, so the card knows a run over all needs
    /// at least two.
    #[serde(default)]
    pub models: usize,
    /// The tasks every run holds, with their limits, for the dialogs and
    /// for results of older runs that lack some.
    #[serde(default)]
    pub tasks: Vec<CodeBenchTaskInfo>,
}

#[server(prefix = "/admin/api", endpoint = "code-bench")]
pub async fn get_code_bench() -> Result<CodeBenchPage, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.code_bench().await)
}

/// Lets the active model solve the three tasks; the PC wakes with the
/// first request.
#[server(prefix = "/admin/api", endpoint = "code-bench-start")]
pub async fn start_code_bench() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.start_code_bench().await.map_err(bad_request)
}

/// Runs the code benchmark for every installed model in turn; the model active
/// before it comes back at the end.
#[server(prefix = "/admin/api", endpoint = "code-bench-start-all")]
pub async fn start_code_bench_all() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.start_code_bench_all().await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "code-bench-cancel")]
pub async fn cancel_code_bench() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()?.cancel_code_bench().await.map_err(bad_request)
}

/// A job as the page asks for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrataJobForm {
    pub kind: StrataJobKind,
    pub family: Option<String>,
    pub size: Option<String>,
    pub context: Option<u32>,
    pub model: Option<String>,
    /// Strata's experimental speed projection, for setup and another model;
    /// on or off for an installed one.
    pub speed_projection: bool,
    /// How a tune looks and what it judges speed by.
    pub tune_mode: TuneMode,
    pub tune_use: TuneUse,
}

// --- History: the PC's states over a range of days ---

/// How much time one bar of the history covers; the proxy picks it from the
/// length of the range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryBucket {
    /// Ranges up to 62 days.
    #[default]
    Day,
    /// Up to 366 days; weeks start on Monday.
    Week,
    Month,
}

/// Where the energy of a bar comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergySource {
    /// From the power plug: every hour with samples for at least half its
    /// minutes.
    Measured,
    /// From the watt settings (energy estimate).
    #[default]
    Estimated,
    /// Some hours measured, some estimated.
    Mixed,
}

/// Energy and savings over a bar or the whole range. € use today's
/// price for all of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Energy {
    pub kwh: f64,
    pub eur: f64,
    /// Against running awake around the clock: asleep hours × (awake W −
    /// asleep W), always from the settings.
    pub saved_kwh: f64,
    pub saved_eur: f64,
    pub source: EnergySource,
}

/// The settings the energy estimate and the € use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PowerSettings {
    pub awake_watts: f64,
    pub asleep_watts: f64,
    pub price_ct_per_kwh: f64,
}

/// One bar of the history: a day, a week or a month, as `HistoryView::bucket`
/// says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryDay {
    /// The first day of the bar in the NAS's time zone, e.g. `2026-10-19`;
    /// for the first bar not before the range's first day.
    pub date: String,
    /// The bar as the page names it: `Sat 17 Oct`, `12 Oct` for a week from
    /// that day, `Oct 2026`.
    pub label: String,
    pub awake_h: f64,
    pub gaming_h: f64,
    pub asleep_h: f64,
    /// Time the proxy could not observe; never counted as sleep or awake.
    pub unknown_h: f64,
    pub energy: Energy,
}

/// The same hours summed over the whole range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryTotals {
    pub awake_h: f64,
    pub gaming_h: f64,
    pub asleep_h: f64,
    pub unknown_h: f64,
    pub energy: Energy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryView {
    /// One entry per bar, oldest first.
    pub days: Vec<HistoryDay>,
    pub bucket: HistoryBucket,
    pub totals: HistoryTotals,
    /// The range's first and last day, `2026-10-13` and `2026-10-19`.
    pub from: String,
    pub to: String,
    pub power: PowerSettings,
    /// False while no PC service is set up: the proxy cannot see what the PC
    /// does on its own, so the history stays incomplete.
    pub agent_configured: bool,
}

/// The history over `range`: `7d` for the last 7 days today included, `12m`
/// for the last twelve months, `all` back to the first event, or two days
/// as `2026-01-01..2026-03-31`, both included.
#[server(prefix = "/admin/api", endpoint = "history")]
pub async fn get_history(range: String) -> Result<HistoryView, ServerFnError> {
    require_login(false).await?;
    Ok(backend()?.history(range).await)
}
