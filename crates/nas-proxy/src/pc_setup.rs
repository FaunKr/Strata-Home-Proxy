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
use strata_admin_ui::api::{PcServiceView, PcSetupCommand};

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
/// Text the program carries in front of its version (see `pc-agent/src/main.rs`).
const VERSION_MARKER: &[u8] = b"strata-pc-agent-version:";
/// An accepted update that has not reported its new version after this long failed.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(180);

pub struct PcSetup {
    program: Mutex<PathBuf>,
    code: Mutex<Option<Code>>,
    lifetime: Mutex<Duration>,
    /// The program's version, with the file's size and time it was read from.
    program_version: Mutex<Option<(FileStamp, Option<String>)>>,
    update: Mutex<UpdateState>,
    update_timeout: Mutex<Duration>,
}

type FileStamp = (u64, Option<std::time::SystemTime>);

#[derive(Default)]
struct UpdateState {
    /// The version in the last status; `Some(None)` from a service too old
    /// to report one.
    reported: Option<Option<String>>,
    /// When the service accepted an update; until it reports the new version.
    running_since: Option<Instant>,
    error: Option<String>,
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
            program_version: Mutex::new(None),
            update: Mutex::new(UpdateState::default()),
            update_timeout: Mutex::new(UPDATE_TIMEOUT),
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

    /// How long an accepted update may take to report back; shorter in tests.
    pub fn set_update_timeout(&self, timeout: Duration) {
        *self.update_timeout.lock().unwrap() = timeout;
    }

    /// The version of the program the NAS serves, read from the file and
    /// read again only when it changes; `None` without a program.
    pub fn program_version(&self) -> Option<String> {
        let path = self.program();
        let meta = std::fs::metadata(&path).ok()?;
        let stamp = (meta.len(), meta.modified().ok());
        let mut cached = self.program_version.lock().unwrap();
        if let Some((s, version)) = cached.as_ref()
            && *s == stamp
        {
            return version.clone();
        }
        let version = std::fs::read(&path)
            .ok()
            .and_then(|bytes| version_in(&bytes));
        *cached = Some((stamp, version.clone()));
        version
    }

    /// Takes note of the version in a status. True once an update arrived:
    /// the service runs the program the NAS carries.
    pub fn note_version(&self, version: Option<&str>) -> bool {
        let available = self.program_version();
        let mut update = self.update.lock().unwrap();
        update.reported = Some(version.map(String::from));
        let arrived =
            update.running_since.is_some() && version.is_some() && version == available.as_deref();
        if arrived {
            update.running_since = None;
        }
        arrived
    }

    /// What the overview shows; `None` until the service answered once.
    /// Ends an update that took too long.
    pub fn view(&self) -> (Option<PcServiceView>, Option<String>) {
        let available = self.program_version();
        let timeout = *self.update_timeout.lock().unwrap();
        let mut update = self.update.lock().unwrap();
        let mut timed_out = None;
        if update.running_since.is_some_and(|t| t.elapsed() >= timeout) {
            update.running_since = None;
            let reason = "Der PC-Dienst hat nach dem Update keine neue Version gemeldet. Läuft er noch? Sonst den Befehl von der Seite PC-Dienst auf dem PC ausführen.".to_string();
            update.error = Some(reason.clone());
            timed_out = Some(reason);
        }
        let view = update.reported.clone().map(|version| PcServiceView {
            update_available: available.is_some() && version != available,
            version,
            available,
            updating: update.running_since.is_some(),
            update_error: update.error.clone(),
        });
        (view, timed_out)
    }

    fn update_started(&self) {
        let mut update = self.update.lock().unwrap();
        update.running_since = Some(Instant::now());
        update.error = None;
    }

    fn update_failed(&self, reason: &str) {
        let mut update = self.update.lock().unwrap();
        update.running_since = None;
        update.error = Some(reason.to_string());
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

/// The version text after the marker, up to the line end.
fn version_in(program: &[u8]) -> Option<String> {
    let start = program
        .windows(VERSION_MARKER.len())
        .position(|w| w == VERSION_MARKER)?
        + VERSION_MARKER.len();
    let rest = &program[start..program.len().min(start + 64)];
    let end = rest.iter().position(|b| *b == b'\n')?;
    let version = std::str::from_utf8(&rest[..end]).ok()?;
    (!version.is_empty() && version.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| version.to_string())
}

/// The update button: asks the awake PC's service to load the program from
/// the NAS. Never wakes the PC.
pub async fn update(state: &Arc<AppState>, by: &str) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(strata_admin_ui::labels::NO_AGENT.into());
    }
    let Some(available) = state.pc_setup.program_version() else {
        return Err("Dieses Image enthält keinen PC-Dienst zum Aktualisieren.".into());
    };
    if state.pc_status(&agent).await.is_none() {
        return Err("Der PC schläft; ein Update geht nur bei wachem PC.".into());
    }
    state
        .events
        .record(EventKind::PcServiceUpdateRequested, Some(by.to_string()));
    let result = agent.update().await;
    match &result {
        Ok(()) => {
            tracing::info!("PC-Dienst wird auf {available} aktualisiert");
            state.pc_setup.update_started();
        }
        Err(reason) => {
            tracing::warn!("Update des PC-Dienstes: {reason}");
            state.pc_setup.update_failed(reason);
            state
                .events
                .record(EventKind::PcServiceUpdateFailed, Some(reason.clone()));
        }
    }
    state.overview.refresh_now();
    // The reason shows on the overview, where it stays.
    Ok(())
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
