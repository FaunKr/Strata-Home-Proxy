//! Setting up the PC service from the NAS: the Windows program the image
//! carries, the one-time code behind the setup command, and the sign-up that
//! trades the code for the token. All under `/admin/agent/`; like the rest of
//! `/admin/`, nothing here wakes the PC or counts as activity. Only the
//! update button does: it wakes a sleeping PC first.

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

use crate::{
    agent::UpdateAnswer,
    events::EventKind,
    files::write_private,
    pc_changelog::{self, Changelog},
    state::AppState,
};

/// Where the image puts `strata-pc-agent.exe`, unless `PC_AGENT_EXE` says otherwise.
const DEFAULT_PROGRAM: &str = "/usr/local/share/strata-proxy/strata-pc-agent.exe";
const CODE_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// Easy to read out and type: no 0/O or 1/I.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;
/// Installs, updates and removes the PC service; also used on its own, next
/// to the program from the CI download.
const INSTALL_SCRIPT: &str = include_str!("../../../deploy/windows/install.ps1");
/// Text the program carries in front of its version (see `pc-agent/src/lib.rs`).
const VERSION_MARKER: &[u8] = b"strata-pc-agent-version:";
/// An update that has not reported its new version this long after the
/// button failed; the service may take 5 min for the download alone.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(420);
const RUNNING: &str = "An update is running already.";
/// The version the PC service reported last, in `DATA_DIR`.
const VERSION_FILE: &str = "pc-service-version.json";
const UPDATE_TIMED_OUT: &str = "The PC service reported no new version after the update. Is it still running? Otherwise run the install command from the PC area on the PC.";

pub struct PcSetup {
    dir: Option<PathBuf>,
    program: Mutex<PathBuf>,
    code: Mutex<Option<Code>>,
    lifetime: Mutex<Duration>,
    /// The program's version, with the file's size and time it was read from.
    program_version: Mutex<Option<(FileStamp, Option<String>)>>,
    update: Mutex<UpdateState>,
    update_timeout: Mutex<Duration>,
    /// Another changelog than the built-in one: from `PC_AGENT_CHANGELOG`
    /// (the UI check) or a test.
    changelog: Mutex<Option<Arc<Changelog>>>,
}

type FileStamp = (u64, Option<std::time::SystemTime>);

#[derive(Default)]
struct UpdateState {
    /// The version in the last status, also from before a restart;
    /// `Some(None)` from a service too old to report one.
    reported: Option<Option<String>>,
    /// When the button asked for an update; until the service reports the
    /// new version, refuses or the update times out.
    running_since: Option<Instant>,
    error: Option<String>,
}

struct Code {
    value: String,
    until: Instant,
}

/// What `VERSION_FILE` holds; `version` is `None` for a service too old to
/// report one.
#[derive(Serialize, Deserialize)]
struct SavedVersion {
    version: Option<String>,
}

impl PcSetup {
    /// Reads the version saved in `dir` before a restart, if any.
    pub fn new(dir: Option<PathBuf>) -> Self {
        let program = std::env::var_os("PC_AGENT_EXE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PROGRAM));
        let reported = dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(VERSION_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice::<SavedVersion>(&bytes).ok())
            .map(|saved| saved.version);
        Self {
            dir,
            program: Mutex::new(program),
            code: Mutex::new(None),
            lifetime: Mutex::new(CODE_LIFETIME),
            program_version: Mutex::new(None),
            update: Mutex::new(UpdateState {
                reported,
                ..UpdateState::default()
            }),
            update_timeout: Mutex::new(UPDATE_TIMEOUT),
            changelog: Mutex::new(changelog_from_env().map(Arc::new)),
        }
    }

    /// The changelog set for a test or the UI check, else the built-in one.
    fn changelog(&self) -> Arc<Changelog> {
        self.changelog
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Arc::clone(&crate::pc_changelog::PARSED))
    }

    /// Uses `text` as the PC service's changelog, e.g. one of a test.
    pub fn set_changelog(&self, text: &str) -> Result<(), String> {
        *self.changelog.lock().unwrap() = Some(Arc::new(Changelog::parse(text)?));
        Ok(())
    }

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
        // A version the changelog cannot compare counts like no program.
        let version = std::fs::read(&path)
            .ok()
            .and_then(|bytes| version_in(&bytes))
            .and_then(|v| pc_changelog::plain(&v));
        *cached = Some((stamp, version.clone()));
        version
    }

    /// Takes note of the version in a status. True once an update arrived:
    /// the service runs the program the NAS carries.
    pub fn note_version(&self, version: Option<&str>) -> bool {
        let available = self.program_version();
        let mut update = self.update.lock().unwrap();
        let reported = Some(version.map(String::from));
        if update.reported != reported {
            update.reported = reported;
            self.save(version);
        }
        let current = available.is_some_and(|a| pc_changelog::runs_at_least(version, &a));
        if !current {
            return false;
        }
        // Up to date, however it got there: an old failure no longer matters.
        update.error = None;
        update.running_since.take().is_some()
    }

    /// Keeps `version` for after a restart; without `DATA_DIR`, nowhere.
    fn save(&self, version: Option<&str>) {
        let Some(dir) = &self.dir else { return };
        let saved = SavedVersion {
            version: version.map(String::from),
        };
        let bytes = serde_json::to_vec(&saved).expect("version serializes");
        if let Err(e) = write_private(dir, VERSION_FILE, &bytes) {
            tracing::warn!("{VERSION_FILE} not saved: {e}");
        }
    }

    /// Whether the service last reported `available` or a higher version.
    fn already_runs(&self, available: &str) -> bool {
        self.update
            .lock()
            .unwrap()
            .reported
            .as_ref()
            .is_some_and(|v| pc_changelog::runs_at_least(v.as_deref(), available))
    }

    /// Ends an update that took too long; its reason if it did.
    fn expire(&self, update: &mut UpdateState) -> Option<String> {
        let timeout = *self.update_timeout.lock().unwrap();
        if !update.running_since.is_some_and(|t| t.elapsed() >= timeout) {
            return None;
        }
        update.running_since = None;
        update.error = Some(UPDATE_TIMED_OUT.to_string());
        Some(UPDATE_TIMED_OUT.to_string())
    }

    /// What the overview shows; `None` until the service answered once.
    /// Also the reason when an update just timed out.
    pub fn view(&self) -> (Option<PcServiceView>, Option<String>) {
        let available = self.program_version();
        let mut update = self.update.lock().unwrap();
        let timed_out = self.expire(&mut update);
        let changelog = self.changelog();
        let view = update.reported.clone().map(|version| {
            let update_available = available
                .as_deref()
                .is_some_and(|a| pc_changelog::is_newer(a, version.as_deref()));
            let changes = if update_available {
                changelog.changes(version.as_deref(), available.as_deref())
            } else {
                Vec::new()
            };
            PcServiceView {
                update_available,
                // Without the build suffix of an older service.
                version: version.map(|v| pc_changelog::plain(&v).unwrap_or(v)),
                available,
                updating: update.running_since.is_some(),
                update_error: update.error.clone(),
                reported_mac: None,
                changes,
            }
        });
        (view, timed_out)
    }

    /// Refuses while an update runs.
    fn check_not_running(&self) -> Result<(), String> {
        let mut update = self.update.lock().unwrap();
        self.expire(&mut update);
        if update.running_since.is_some() {
            return Err(RUNNING.into());
        }
        Ok(())
    }

    /// Marks an update as asked for, unless one runs or the service already
    /// runs `available` or a higher version.
    fn begin_update(&self, available: &str) -> Result<(), String> {
        let mut update = self.update.lock().unwrap();
        self.expire(&mut update);
        if update.running_since.is_some() {
            return Err(RUNNING.into());
        }
        if update
            .reported
            .as_ref()
            .is_some_and(|v| pc_changelog::runs_at_least(v.as_deref(), available))
        {
            return Err(
                "The PC service already runs the version on the NAS or a newer one.".into(),
            );
        }
        update.running_since = Some(Instant::now());
        update.error = None;
        Ok(())
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
                "The address {host:?} does not work for the command; open this page through the NAS's IP address."
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

/// The changelog `PC_AGENT_CHANGELOG` names; `None` without one, or when it
/// cannot be read or is invalid (then the built-in one applies).
fn changelog_from_env() -> Option<Changelog> {
    let path = std::env::var_os("PC_AGENT_CHANGELOG")?;
    let parsed = std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| Changelog::parse(&text));
    parsed
        .inspect_err(|e| tracing::warn!("PC_AGENT_CHANGELOG {path:?} not used: {e}"))
        .ok()
}

/// The version text after the marker, up to the line end. The prefix may
/// also stand elsewhere in the program on its own; only a match followed by a
/// plain version and a line end counts.
fn version_in(program: &[u8]) -> Option<String> {
    program
        .windows(VERSION_MARKER.len())
        .enumerate()
        .filter(|(_, w)| *w == VERSION_MARKER)
        .find_map(|(at, _)| {
            let start = at + VERSION_MARKER.len();
            let rest = &program[start..program.len().min(start + 64)];
            let end = rest.iter().position(|b| *b == b'\n')?;
            let version = std::str::from_utf8(&rest[..end]).ok()?;
            (!version.is_empty() && version.bytes().all(|b| b.is_ascii_graphic()))
                .then(|| version.to_string())
        })
}

/// The update button: asks the PC's service to load the program from the
/// NAS, waking a sleeping PC first. The wake counts as use, so the PC sleeps
/// again after the idle timeout.
pub async fn update(state: &Arc<AppState>, by: &str) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(strata_admin_ui::labels::NO_AGENT.into());
    }
    let Some(available) = state.pc_setup.program_version() else {
        return Err("This image carries no PC service to update.".into());
    };
    let requested = || {
        state
            .events
            .record(EventKind::PcServiceUpdateRequested, Some(by.to_string()));
    };
    let asleep = state.ask_pc(&agent).await.is_none();
    if asleep {
        // Checked before anything is recorded: a press that cannot wake the
        // PC, or one during an update's restart, leaves no trace.
        if let Some(why) = crate::strata::cannot_wake(state) {
            return Err(why.into());
        }
        state.pc_setup.check_not_running()?;
        // Recorded first, so the wake shows up with its reason.
        requested();
        state.touch();
        crate::strata::wake(state, &format!("for the PC service update, {by}")).await?;
        // The saved version may be stale: the fresh status decides.
        state
            .ask_pc(&agent)
            .await
            .ok_or("The PC service does not answer.")?;
        if state.pc_setup.already_runs(&available) {
            state.overview.refresh_now();
            return Ok(());
        }
    }
    state.pc_setup.begin_update(&available)?;
    if !asleep {
        requested();
    }
    state.overview.refresh_now();
    match agent.update().await {
        UpdateAnswer::Accepted => tracing::info!("PC service is updating to {available}"),
        // It may still finish; the new version or the timeout tells.
        UpdateAnswer::NoAnswer => {
            tracing::warn!("PC service did not answer the update, waiting for the new version")
        }
        UpdateAnswer::Refused(reason) => {
            tracing::warn!("Update of the PC service: {reason}");
            state.pc_setup.update_failed(&reason);
            state
                .events
                .record(EventKind::PcServiceUpdateFailed, Some(reason));
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
            tracing::warn!("{} not readable: {e}", path.display());
            (StatusCode::NOT_FOUND, "This image carries no PC service").into_response()
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
            "The NAS address in the command does not work. Open the web UI through the NAS's IP address and create a new command.",
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

const CODE_INVALID: &str =
    "The code is not valid (any more). Create a new command in the PC area of the web UI.";

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
    /// The port of the proxy's Strata address, which the install command
    /// opens for the NAS.
    strata_port: u16,
}

/// The port the install command opens for Strata: the one the proxy's
/// Strata address reaches, or Strata's default without an address.
fn strata_port(strata_url: Option<&str>) -> u16 {
    strata_url
        .and_then(|url| reqwest::Url::parse(url).ok())
        .and_then(|url| url.port_or_known_default())
        .unwrap_or(8080)
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
        return refused("The NAS address does not work; use the NAS's IP address.");
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
        tracing::error!("PC service not saved in the settings: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Not saved: {e}")})),
        )
            .into_response();
    }
    state
        .events
        .record(EventKind::PcServiceRegistered, Some(url.clone()));
    state.overview.refresh_now();
    tracing::info!("PC service set up at {url}");
    Json(Registered {
        token,
        nas_url: format!("http://{host}"),
        strata_port: strata_port(state.cfg().strata_url.as_deref()),
    })
    .into_response()
}

fn refused(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": message}))).into_response()
}
