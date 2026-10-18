//! Server functions: the HTTP interface between the pages and the proxy, all
//! under `/admin/api/`.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ssr")]
use crate::server::{backend, clear_cookie, client_ip, require_login, set_cookie, token};

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
        return Err(ServerFnError::new(
            "Die beiden neuen Passwörter stimmen nicht überein.",
        ));
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
    /// The value in effect; for secrets only "gesetzt".
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
}

#[server(prefix = "/admin/api", endpoint = "settings")]
pub async fn get_settings() -> Result<SettingsView, ServerFnError> {
    require_login(false).await?;
    Ok(backend().settings())
}

#[server(prefix = "/admin/api", endpoint = "save-setting")]
pub async fn save_setting(key: String, value: String) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().save_setting(&key, &value).map_err(bad_request)
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

/// What the overview shows; sent as a whole on every change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    /// The proxy's clock (Unix seconds), so the page can count down without
    /// trusting the viewer's clock.
    pub now: u64,
    /// False while no Strata URL is set.
    pub configured: bool,
    /// False while no MAC address is known: the PC cannot be woken.
    pub mac_known: bool,
    /// False without a PC service: no gaming mode and no automatic sleep.
    pub agent_configured: bool,
    /// False while no API key exists: every client gets 401.
    pub api_key_exists: bool,
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
}

/// Kinds of events in the history; their snake_case names are what
/// `events.jsonl` stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WakeSent,
    Awake,
    WakeTimeout,
    SleepAccepted,
    SleepRefused,
    GamingRejected,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataFamily {
    pub id: String,
    pub title: String,
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
) -> Result<(), ServerFnError> {
    require_login(false).await?;
    let form = StrataJobForm {
        kind,
        family,
        size,
        context,
        model,
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
}
