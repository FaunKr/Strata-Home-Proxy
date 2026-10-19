//! Server functions: the HTTP interface between the pages and the proxy, all
//! under `/admin/api/`.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

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
    Ok(backend().session(token().await.as_deref()))
}

#[server(prefix = "/admin/api", endpoint = "login")]
pub async fn login(password: String) -> Result<(), ServerFnError> {
    let token = backend().login(&password, client_ip().await).map_err(|e| {
        expect_context::<leptos_axum::ResponseOptions>()
            .set_status(axum::http::StatusCode::UNAUTHORIZED);
        ServerFnError::new(e)
    })?;
    set_cookie(&token);
    Ok(())
}

#[server(prefix = "/admin/api", endpoint = "logout")]
pub async fn logout() -> Result<(), ServerFnError> {
    if let Some(token) = token().await {
        backend().logout(&token);
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
    let token = backend()
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
    Ok(backend().settings())
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
    backend()
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
    backend().reset_setting(&key).map_err(bad_request)
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
    Ok(backend().api_keys())
}

#[server(prefix = "/admin/api", endpoint = "create-api-key")]
pub async fn create_api_key(name: String) -> Result<CreatedKey, ServerFnError> {
    require_login(false).await?;
    backend().create_api_key(&name).map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "revoke-api-key")]
pub async fn revoke_api_key(name: String, prefix: String) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend()
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
    backend()
        .pc_setup_command(host.as_deref().unwrap_or_default())
        .map_err(bad_request)
}

#[cfg(feature = "ssr")]
fn bad_request(message: String) -> ServerFnError {
    expect_context::<leptos_axum::ResponseOptions>()
        .set_status(axum::http::StatusCode::BAD_REQUEST);
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
    Ok(backend().setup())
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
    /// The Verlauf of the last 7 days, summed at most once a minute.
    pub week: HistoryView,
}

/// The Leistung of the Messsteckdose for the overview; independent of the PC's
/// state, since the plug also answers while the PC sleeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PowerView {
    /// No Messsteckdose is set up (`SHELLY_URL`).
    NotConfigured,
    /// The Messsteckdose did not answer usefully.
    Unreachable,
    /// The current Leistung in watts.
    Watts(f64),
}

/// The Hardwarewerte for the overview.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HardwareView {
    /// No values: the PC is not awake, or its service does not answer.
    Unavailable,
    /// The PC service answers but is too old to report them.
    Unsupported,
    Values(Hardware),
}

/// What the PC service reports about GPU and CPU in `/status` (see
/// docs/pc-agent-api.md). A value it could not read is `None`.
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

/// One message of the Strata-Ausgabe stream (`/admin/api/strata-output-stream`).
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

/// Why the Strata-Ausgabe stands still, or `Running` when it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputNotice {
    Running,
    Asleep,
    NoAgent,
    /// The PC service is too old for the Strata-Ausgabe.
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
    /// The proxy made a new Strata key and Strata took it over.
    StrataKeyChanged,
    /// A PC signed up with a one-time code (detail: the PC service's address).
    PcServiceRegistered,
    /// The update button (detail: who); then the new version or why it failed.
    PcServiceUpdateRequested,
    PcServiceUpdated,
    PcServiceUpdateFailed,
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
    /// overview, where the Ereignisse that caused them already stand.
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
    backend().wake().map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "sleep-now")]
pub async fn sleep_now() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().sleep_now().await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "gaming")]
pub async fn set_gaming(enabled: bool) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().set_gaming(enabled).await.map_err(bad_request)
}

/// Updates the PC service to the program the NAS carries; only while the PC
/// is awake, never wakes it.
#[server(prefix = "/admin/api", endpoint = "pc-service-update")]
pub async fn update_pc_service() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().update_pc_service().await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "keep-awake")]
pub async fn keep_awake(choice: KeepAwakeChoice) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().keep_awake(choice).map_err(bad_request)
}

// --- Strata on the PC: setup, models, update, benchmark ---

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

/// A job on the PC as its service reports it (see docs/pc-agent-api.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataJob {
    /// Counts up per job on the PC; missing from older services.
    #[serde(default)]
    pub id: Option<u64>,
    pub kind: StrataJobKind,
    #[serde(default)]
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
    /// A benchmark's result.
    #[serde(default)]
    pub result: Option<String>,
}

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
    /// Its config loads the speed projection's control vector; `None` from
    /// an older PC service, which does not say.
    #[serde(default)]
    pub speed_projection: Option<bool>,
    /// Tokens it starts with; `None` from an older PC service, which does
    /// not say, or a config without it.
    #[serde(default)]
    pub context: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataFamily {
    pub id: String,
    pub title: String,
    /// Setup offers the experimental speed projection; an older PC service
    /// does not say so.
    #[serde(default)]
    pub speed_projection: bool,
}

/// A size from Strata's catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataSize {
    pub id: String,
    pub family: String,
    pub size: String,
    pub download_gb: f64,
    pub ram_gb: u32,
    /// The PC has the RAM for it.
    pub fits: bool,
    /// Parts are downloaded already; installing continues there.
    pub partly_downloaded: bool,
}

/// The last benchmark run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataBenchmark {
    pub model: String,
    /// Tokens per second as the benchmark reported them.
    pub result: String,
    /// Unix seconds.
    pub at: u64,
    /// Local time text, filled in by the proxy.
    #[serde(default)]
    pub at_text: String,
}

/// Strata on the PC as its service reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataInfo {
    pub installed: bool,
    /// Short commit of the Strata folder.
    #[serde(default)]
    pub version: Option<String>,
    /// Whether the source has newer commits; `None` if unknown.
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
    #[serde(default)]
    pub disk_free_gb: Option<f64>,
    pub data_dir: String,
    #[serde(default)]
    pub benchmark: Option<StrataBenchmark>,
    #[serde(default)]
    pub job: Option<StrataJob>,
}

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
}

#[server(prefix = "/admin/api", endpoint = "strata")]
pub async fn get_strata() -> Result<StrataPage, ServerFnError> {
    require_login(false).await?;
    Ok(backend().strata().await)
}

/// Starts a job on the PC, waking it first if it sleeps. The proxy adds the
/// Strata key where the job needs one; it never reaches the page.
#[server(prefix = "/admin/api", endpoint = "strata-job")]
pub async fn start_strata_job(
    kind: StrataJobKind,
    family: Option<String>,
    size: Option<String>,
    context: Option<u32>,
    model: Option<String>,
    speed_projection: Option<bool>,
) -> Result<(), ServerFnError> {
    require_login(false).await?;
    let form = StrataJobForm {
        kind,
        family,
        size,
        context,
        model,
        speed_projection: speed_projection.unwrap_or(false),
    };
    backend().start_strata_job(form).await.map_err(bad_request)
}

#[server(prefix = "/admin/api", endpoint = "strata-cancel")]
pub async fn cancel_strata_job() -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().cancel_strata_job().await.map_err(bad_request)
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
}

// --- Verlauf: the PC's states over a range of days ---

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
}

/// The same hours summed over the whole range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryTotals {
    pub awake_h: f64,
    pub gaming_h: f64,
    pub asleep_h: f64,
    pub unknown_h: f64,
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
    /// The day of the first Ereignis, where "All" begins; none on a fresh
    /// install.
    pub first_day: Option<String>,
    /// False while no PC service is set up: the proxy cannot see what the PC
    /// does on its own, so the history stays incomplete.
    pub agent_configured: bool,
}

/// The history over `range`: `7d` for the last 7 days today included, `12m`
/// for the last twelve months, `all` back to the first Ereignis, or two days
/// as `2026-01-01..2026-03-31`, both included.
#[server(prefix = "/admin/api", endpoint = "history")]
pub async fn get_history(range: String) -> Result<HistoryView, ServerFnError> {
    require_login(false).await?;
    Ok(backend().history(range).await)
}
