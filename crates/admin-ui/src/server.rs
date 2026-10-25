//! Server side: the axum routes for `/admin/` and the interface the proxy
//! implements for the server functions.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use axum::{
    Router,
    extract::{ConnectInfo, Query},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures::{StreamExt, future::BoxFuture, stream::BoxStream};
use leptos::prelude::*;
use leptos_axum::{
    AxumRouteListing, LeptosRoutes, ResponseOptions,
    generate_route_list_with_exclusions_and_ssg_and_context,
};

use crate::{
    api::{
        ApiKeysView, BenchmarkKind, CodeBenchPage, CreatedKey, ForcedRestart, HistoryView,
        KeepAwakeChoice, KeyModelName, OutputMessage, Overview, PcSetupCommand, Session,
        SettingValue, SettingsView, Setup, SleepAnswer, StrataJobForm, StrataPage, history_csv,
    },
    app::{App, shell},
};

pub const COOKIE: &str = "strata_admin";
const COOKIE_MAX_AGE_SECS: u64 = 30 * 24 * 3600;

/// What the UI needs from the proxy. Provided as context to every page and
/// server function call.
pub trait Backend: Send + Sync + 'static {
    fn session(&self, token: Option<&str>) -> Session;
    /// A session token, or a message for the user.
    fn login(&self, password: String, client: IpAddr)
    -> BoxFuture<'static, Result<String, String>>;
    /// A new session token, or a message for the user.
    fn change_password(
        &self,
        token: String,
        current: String,
        new: String,
        client: IpAddr,
    ) -> BoxFuture<'static, Result<String, String>>;
    /// Ends the session of this token, also for copies of the cookie.
    fn logout(&self, token: &str);
    fn settings(&self) -> SettingsView;
    /// Saves values together, leaving empty secrets as they are; for an
    /// invalid value its key and a message for the user, and none is saved.
    fn save_settings(&self, values: &[SettingValue]) -> Result<(), (Option<String>, String)>;
    /// Drops a saved value, so the environment or default applies again.
    fn reset_setting(&self, key: &str) -> Result<(), String>;
    /// How far the proxy is set up.
    fn setup(&self) -> Setup;
    /// The overview now and after every change, for as long as it is held.
    /// While anyone holds one, the proxy checks on the PC and Strata.
    fn overview(&self) -> BoxStream<'static, Overview>;
    /// The Strata-Ausgabe for as long as it is held; never wakes the PC.
    fn strata_output(&self) -> BoxStream<'static, OutputMessage>;
    /// Starts waking the PC; a message for the user if it cannot be woken.
    fn wake(&self) -> Result<(), String>;
    /// Asks the PC service to sleep: its answer, with the reason of a
    /// refusal; `Err` when it could not be asked.
    fn sleep_now(&self) -> BoxFuture<'static, Result<SleepAnswer, String>>;
    /// Asks the awake PC's service for a forced sleep; never wakes the PC.
    fn force_sleep(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Asks the awake PC's service for a forced Strata restart; never wakes
    /// the PC.
    fn force_restart_strata(&self) -> BoxFuture<'static, Result<ForcedRestart, String>>;
    /// Switches gaming mode, waking the PC first if needed.
    fn set_gaming(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>>;
    fn keep_awake(&self, choice: KeepAwakeChoice) -> Result<(), String>;
    fn api_keys(&self) -> ApiKeysView;
    /// Creates a client key; a message for the user if the name is invalid.
    fn create_api_key(&self, name: &str, model_name: KeyModelName) -> Result<CreatedKey, String>;
    /// Removes a client key; a message for the user if there is none.
    fn revoke_api_key(&self, name: &str, prefix: &str) -> Result<(), String>;
    /// Sets whether apps with a key see the public model name; a message
    /// for the user if there is no such key.
    fn set_api_key_model_name(
        &self,
        name: &str,
        prefix: &str,
        model_name: KeyModelName,
    ) -> Result<(), String>;
    /// Strata on the PC, without waking it.
    fn strata(&self) -> BoxFuture<'static, StrataPage>;
    /// Starts a Strata job, waking the PC first; the PC service's reason if
    /// it refuses.
    fn start_strata_job(&self, form: StrataJobForm) -> BoxFuture<'static, Result<(), String>>;
    fn cancel_strata_job(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Starts the job that switches relay mode; why not if it cannot, never
    /// waking the PC.
    fn set_relay(&self, on: bool) -> BoxFuture<'static, Result<(), String>>;
    /// The optimization prompt for an installed model, never waking the PC;
    /// why not if there is none.
    fn optimization_prompt(&self, model: String) -> BoxFuture<'static, Result<String, String>>;
    /// Has Strata write the update summary now, waking the PC first; why
    /// not if it cannot start.
    fn generate_update_summary(&self) -> BoxFuture<'static, Result<(), String>>;
    /// The code benchmark: its state and each model's last result.
    fn code_bench(&self) -> BoxFuture<'static, CodeBenchPage>;
    /// Starts a code benchmark of the active model; a message if it cannot.
    fn start_code_bench(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Runs the code benchmark for every installed model in turn; a message if
    /// it cannot start now.
    fn start_code_bench_all(&self) -> BoxFuture<'static, Result<(), String>>;
    fn cancel_code_bench(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Deletes one run of `benchmark`; a message if there is no such run.
    /// Never wakes the PC.
    fn delete_benchmark_run(&self, benchmark: BenchmarkKind, id: u64) -> Result<(), String>;
    /// Deletes all runs of `benchmark` for `model`; refused while the PC
    /// service's last report lists the model. Never wakes the PC.
    fn delete_benchmark_model(&self, benchmark: BenchmarkKind, model: &str) -> Result<(), String>;
    /// A new one-time code and the command with it, for the PC to reach the
    /// NAS at `host` (as the browser did); a message if that is not usable.
    fn pc_setup_command(&self, host: &str) -> Result<PcSetupCommand, String>;
    /// Asks the PC's service to update itself from the NAS, waking a sleeping
    /// PC first. A message if the PC cannot be woken or nothing can be
    /// updated; how the update went shows on the overview.
    fn update_pc_service(&self) -> BoxFuture<'static, Result<(), String>>;
    /// How long the PC was awake, gaming, asleep and unobserved over `range`,
    /// as `api::get_history` takes it.
    fn history(&self, range: String) -> BoxFuture<'static, HistoryView>;
}

pub type SharedBackend = Arc<dyn Backend>;

/// The backend from the request's context. A server function that a resource
/// runs while the page renders can lose it: when the part of the page that
/// made the resource is drawn again during the same request, its old owner
/// goes and takes the context with it. That call must end with an error, not
/// a panic: a panicking resource never resolves, and the response with it.
pub fn backend() -> Result<SharedBackend, ServerFnError> {
    use_context::<SharedBackend>()
        .ok_or_else(|| ServerFnError::new("No backend in this request's context."))
}

/// The session token from the request's cookie.
pub async fn token() -> Option<String> {
    let headers: HeaderMap = leptos_axum::extract().await.ok()?;
    cookie_token(&headers)
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE).then(|| value.to_string())
        })
}

/// The `Host` the browser used to reach the page.
pub async fn request_host() -> Option<String> {
    let headers: HeaderMap = leptos_axum::extract().await.ok()?;
    let host = headers.get(header::HOST)?.to_str().ok()?;
    Some(host.to_string())
}

pub async fn client_ip() -> IpAddr {
    leptos_axum::extract::<ConnectInfo<SocketAddr>>()
        .await
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip())
}

/// The caller's token if logged in, else 401. `allow_must_change` lets a
/// session with the default password through (only for changing it).
pub async fn require_login(allow_must_change: bool) -> Result<String, ServerFnError> {
    let token = token().await;
    match backend()?.session(token.as_deref()) {
        Session::Ready => Ok(token.unwrap_or_default()),
        Session::MustChange if allow_must_change => Ok(token.unwrap_or_default()),
        Session::MustChange => {
            set_status(StatusCode::FORBIDDEN);
            Err(ServerFnError::new("Please change the password first."))
        }
        Session::Anonymous => {
            set_status(StatusCode::UNAUTHORIZED);
            Err(ServerFnError::new("Not signed in."))
        }
    }
}

/// The answer's status; without a response in context (see [`backend`])
/// there is none to set.
pub fn set_status(code: StatusCode) {
    if let Some(response) = use_context::<ResponseOptions>() {
        response.set_status(code);
    }
}

pub fn set_cookie(token: &str) {
    let value = format!(
        "{COOKIE}={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_SECS}"
    );
    insert_cookie(&value);
}

pub fn clear_cookie() {
    insert_cookie(&format!(
        "{COOKIE}=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0"
    ));
}

fn insert_cookie(value: &str) {
    if let Ok(value) = HeaderValue::from_str(value)
        && let Some(response) = use_context::<ResponseOptions>()
    {
        response.insert_header(header::SET_COOKIE, value);
    }
}

/// Lets only a logged-in user through who needs no password change, as
/// [`require_login`] does for the server functions.
fn ready_session(backend: &SharedBackend, headers: &HeaderMap) -> Result<(), StatusCode> {
    match backend.session(cookie_token(headers).as_deref()) {
        Session::Ready => Ok(()),
        Session::MustChange => Err(StatusCode::FORBIDDEN),
        Session::Anonymous => Err(StatusCode::UNAUTHORIZED),
    }
}

#[derive(serde::Deserialize)]
struct CsvQuery {
    range: Option<String>,
}

/// The History of `range` as a CSV file, for the download link on the
/// History page. Without a range it is the history's own default.
async fn history_csv_file(
    backend: SharedBackend,
    headers: HeaderMap,
    Query(query): Query<CsvQuery>,
) -> Response {
    if let Err(refused) = ready_session(&backend, &headers) {
        return refused.into_response();
    }
    let view = backend.history(query.range.unwrap_or_default()).await;
    let disposition = format!(
        "attachment; filename=\"history-{}-{}.csv\"",
        view.from, view.to
    );
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition)
                    .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
            ),
        ],
        history_csv(&view),
    )
        .into_response()
}

/// The brand mark from the header as the browser tab's icon.
const FAVICON: &str = include_str!("../icons/favicon.svg");
/// The brand mark as a full square for a phone's home screen; rendered once
/// from `favicon.svg`, so a change to one changes both.
const TOUCH_ICON: &[u8] = include_bytes!("../icons/apple-touch-icon.png");

/// An icon for the browser, without a login: the login page shows it too.
fn icon(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        body,
    )
        .into_response()
}

/// A backend stream as server-sent events, only for a logged-in user.
fn session_stream<T: serde::Serialize + Send + 'static>(
    backend: &SharedBackend,
    headers: &HeaderMap,
    stream: impl FnOnce(&dyn Backend) -> BoxStream<'static, T>,
) -> Response {
    if let Err(refused) = ready_session(backend, headers) {
        return refused.into_response();
    }
    let events = stream(backend.as_ref()).map(|item| {
        Event::default()
            .json_data(item)
            .map_err(|e| std::io::Error::other(e.to_string()))
    });
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Where the built browser files live (`LEPTOS_SITE_ROOT`, set in the Docker
/// image); `target/site` when developing with cargo-leptos.
pub fn site_root() -> PathBuf {
    std::env::var("LEPTOS_SITE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target/site"))
}

pub fn options(site_root: PathBuf) -> LeptosOptions {
    LeptosOptions::builder()
        .output_name("admin-ui")
        .site_root(site_root.to_string_lossy().to_string())
        .site_pkg_dir("admin/pkg")
        .build()
}

/// The paths of all server functions, so a test can call every one.
pub fn server_fn_paths() -> Vec<&'static str> {
    leptos::server_fn::axum::server_fn_paths()
        .map(|(path, _)| path)
        .collect()
}

/// Routes for the pages, the server functions and the browser files.
pub fn router<S>(options: LeptosOptions, backend: SharedBackend) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let pkg = PathBuf::from(options.site_root.as_ref()).join(options.site_pkg_dir.as_ref());
    let overview_backend = Arc::clone(&backend);
    let output_backend = Arc::clone(&backend);
    let csv_backend = Arc::clone(&backend);
    let context = move || provide_context(Arc::clone(&backend));
    // Listing the routes renders the app once, which starts the session
    // resource; without the backend in context that call panics. Meanwhile
    // Leptos holds back the resources of every page the process serves (a
    // global flag), so the list is made once per process: tests build a
    // router each while other tests' pages render (#69).
    static ROUTES: OnceLock<Vec<AxumRouteListing>> = OnceLock::new();
    let routes = ROUTES
        .get_or_init(|| {
            generate_route_list_with_exclusions_and_ssg_and_context(App, None, context.clone()).0
        })
        .clone();
    Router::new()
        // The overview lives at `/admin`; the router knows no trailing slash.
        .route(
            "/admin/",
            get(|| async { axum::response::Redirect::temporary("/admin") }),
        )
        .route(
            "/admin/api/overview-stream",
            get(move |headers: HeaderMap| async move {
                session_stream(&overview_backend, &headers, |b| b.overview())
            }),
        )
        .route(
            "/admin/api/strata-output-stream",
            get(move |headers: HeaderMap| async move {
                session_stream(&output_backend, &headers, |b| b.strata_output())
            }),
        )
        .route(
            "/admin/favicon.svg",
            get(|| async { icon("image/svg+xml", FAVICON.as_bytes()) }),
        )
        .route(
            "/admin/apple-touch-icon.png",
            get(|| async { icon("image/png", TOUCH_ICON) }),
        )
        .route(
            "/admin/history.csv",
            get(move |headers: HeaderMap, query: Query<CsvQuery>| {
                history_csv_file(Arc::clone(&csv_backend), headers, query)
            }),
        )
        .leptos_routes_with_context(&options, routes, context, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .nest_service("/admin/pkg", tower_http::services::ServeDir::new(pkg))
        .with_state(options)
}
