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
    pub pc: PcView,
    pub strata: StrataView,
    /// Start or end of the last API request, as local time text.
    pub last_request: Option<String>,
    pub sleep: SleepView,
    /// End of "keep awake", as local time text.
    pub keep_awake_until: Option<String>,
    /// The latest PC events, newest first.
    pub events: Vec<EventView>,
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
    /// The event's kind, e.g. `wake_sent`.
    pub kind: String,
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

/// `choice` is `1h`, `4h`, `8h`, `morning` (until 8 o'clock) or `off`.
#[server(prefix = "/admin/api", endpoint = "keep-awake")]
pub async fn keep_awake(choice: String) -> Result<(), ServerFnError> {
    require_login(false).await?;
    backend().keep_awake(&choice).map_err(bad_request)
}
