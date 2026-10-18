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
