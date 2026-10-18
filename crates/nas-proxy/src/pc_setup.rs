//! Setting up the PC service from the NAS: the Windows program the image
//! carries, the one-time code behind the setup command, and the sign-up that
//! trades the code for the token. All under `/admin/agent/`; like the rest of
//! `/admin/`, nothing here wakes the PC or counts as activity.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_admin_ui::api::PcSetupCommand;

use crate::{events::EventKind, state::AppState};

/// Where the image puts `strata-pc-agent.exe`, unless `PC_AGENT_EXE` says otherwise.
const DEFAULT_PROGRAM: &str = "/usr/local/share/strata-proxy/strata-pc-agent.exe";
const CODE_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// Easy to read out and type: no 0/O or 1/I.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;
/// Installs, updates and removes the PC service; also used on its own, next
/// to the program from the CI download.
const INSTALL_SCRIPT: &str = include_str!("../../../deploy/windows/install.ps1");

pub struct PcSetup {
    program: Mutex<PathBuf>,
    code: Mutex<Option<Code>>,
    lifetime: Mutex<Duration>,
}

struct Code {
    value: String,
    until: Instant,
}

impl Default for PcSetup {
    fn default() -> Self {
        let program = std::env::var_os("PC_AGENT_EXE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PROGRAM));
        Self {
            program: Mutex::new(program),
            code: Mutex::new(None),
            lifetime: Mutex::new(CODE_LIFETIME),
        }
    }
}

impl PcSetup {
    /// Serves another file as the program, e.g. a stand-in in tests.
    pub fn set_program(&self, path: PathBuf) {
        *self.program.lock().unwrap() = path;
    }

    /// How long new codes stay valid; shorter in tests.
    pub fn set_code_lifetime(&self, lifetime: Duration) {
        *self.lifetime.lock().unwrap() = lifetime;
    }

    fn program(&self) -> PathBuf {
        self.program.lock().unwrap().clone()
    }

    /// A new code and the command around it; the previous code stops working.
    /// The code lives only in memory, so a restart ends it too.
    pub fn command(&self, host: &str) -> Result<PcSetupCommand, String> {
        if !valid_host(host) {
            return Err(format!(
                "Die Adresse {host:?} taugt nicht für den Befehl; die Seite über die IP des NAS öffnen."
            ));
        }
        let lifetime = *self.lifetime.lock().unwrap();
        let value = new_code();
        *self.code.lock().unwrap() = Some(Code {
            value: value.clone(),
            until: Instant::now() + lifetime,
        });
        Ok(PcSetupCommand {
            command: format!("irm http://{host}/admin/agent/install.ps1?code={value} | iex"),
            code: value,
            valid_minutes: lifetime.as_secs().div_ceil(60),
            uninstall_command: format!("irm http://{host}/admin/agent/install.ps1?uninstall | iex"),
        })
    }

    /// Whether `code` is the current code and still valid, without using it up.
    fn is_current(&self, code: &str) -> bool {
        Self::matches(self.code.lock().unwrap().as_ref(), code)
    }

    /// Uses up the code if it is the current one and still valid.
    fn redeem(&self, code: &str) -> bool {
        let mut current = self.code.lock().unwrap();
        let valid = Self::matches(current.as_ref(), code);
        if valid {
            *current = None;
        }
        valid
    }

    fn matches(current: Option<&Code>, code: &str) -> bool {
        current
            .is_some_and(|c| c.value.eq_ignore_ascii_case(code.trim()) && Instant::now() < c.until)
    }
}

fn new_code() -> String {
    crate::auth::random_key()
        .iter()
        .take(CODE_LEN)
        .map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char)
        .collect()
}

/// Host and port as in a URL; nothing that could change the pasted command.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin/agent/strata-pc-agent.exe", get(program))
        .route("/admin/agent/install.ps1", get(install_script))
        .route("/admin/agent/register", post(register))
}

/// The program, without login: it holds no secret.
async fn program(State(state): State<Arc<AppState>>) -> Response {
    let path = state.pc_setup.program();
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"strata-pc-agent.exe\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("{} nicht lesbar: {e}", path.display());
            (
                StatusCode::NOT_FOUND,
                "Der PC-Dienst liegt nicht in diesem Image",
            )
                .into_response()
        }
    }
}

/// The install script with the NAS's address and the code filled in, only
/// for a valid code; `?uninstall` needs none. Loading it leaves the code for
/// the sign-up, so the script can load itself again with administrator rights.
async fn install_script(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let host = host(&headers);
    let uninstall = query.contains_key("uninstall");
    let code = query.get("code").map(String::as_str).unwrap_or_default();
    // Windows PowerShell shows no body for an error status, so an
    // explanation comes as a script that prints it.
    if !valid_host(host) {
        return explain(
            "Die Adresse des NAS im Befehl taugt nicht. Die Weboberflaeche ueber die IP des NAS oeffnen und einen neuen Befehl erzeugen.",
        );
    }
    if !uninstall && !state.pc_setup.is_current(code) {
        return explain(CODE_INVALID);
    }
    let code = if uninstall { "" } else { code.trim() };
    let mut script = INSTALL_SCRIPT
        .replacen(
            "[string]$NasUrl = \"\"",
            &format!("[string]$NasUrl = \"http://{host}\""),
            1,
        )
        .replacen(
            "[string]$Code = \"\"",
            &format!("[string]$Code = \"{}\"", code.to_ascii_uppercase()),
            1,
        );
    if uninstall {
        script = script.replacen("[switch]$Uninstall,", "[switch]$Uninstall = $true,", 1);
    }
    powershell(script)
}

const CODE_INVALID: &str = "Der Code gilt nicht (mehr). Auf der Seite PC-Dienst der Weboberflaeche einen neuen Befehl erzeugen.";

fn explain(message: &str) -> Response {
    powershell(format!("Write-Host '{message}' -ForegroundColor Red"))
}

fn powershell(script: String) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        script,
    )
        .into_response()
}

fn host(headers: &HeaderMap) -> &str {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct Registration {
    code: String,
    /// The port the PC service listens on.
    port: u16,
}

#[derive(Serialize)]
struct Registered {
    /// Shared secret for the PC service and the proxy.
    token: String,
    /// How the PC reaches the NAS, for later downloads.
    nas_url: String,
    /// Strata's key, so the PC service can ask Strata about its health.
    #[serde(skip_serializing_if = "Option::is_none")]
    strata_api_key: Option<String>,
}

/// Trades a valid code for a new token and enters the token and the address
/// of the PC service (the caller, on the port it names) in the settings.
async fn register(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(reg): Json<Registration>,
) -> Response {
    let host = host(&headers);
    if !valid_host(host) {
        return refused("Die Adresse des NAS taugt nicht; die IP des NAS verwenden.");
    }
    if !state.pc_setup.redeem(&reg.code) {
        return refused(CODE_INVALID);
    }
    let token = crate::auth::hex(&crate::auth::random_key());
    let agent = SocketAddr::new(peer.ip().to_canonical(), reg.port);
    let url = format!("http://{agent}");
    let entered = state
        .settings
        .set("PC_AGENT_TOKEN", &token)
        .and_then(|()| state.settings.set("PC_AGENT_URL", &url));
    if let Err(e) = entered {
        tracing::error!("PC-Dienst nicht eingetragen: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Nicht gespeichert: {e}")})),
        )
            .into_response();
    }
    state
        .events
        .record(EventKind::PcServiceRegistered, Some(url.clone()));
    state.overview.refresh_now();
    tracing::info!("PC-Dienst unter {url} eingerichtet");
    Json(Registered {
        token,
        nas_url: format!("http://{host}"),
        strata_api_key: state.cfg().strata_api_key.clone(),
    })
    .into_response()
}

fn refused(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": message}))).into_response()
}
