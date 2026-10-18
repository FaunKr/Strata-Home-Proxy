//! Server side: the axum routes for `/admin/` and the interface the proxy
//! implements for the server functions.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use axum::{
    Router,
    extract::ConnectInfo,
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
    LeptosRoutes, ResponseOptions, generate_route_list_with_exclusions_and_ssg_and_context,
};

use crate::{
    api::{KeepAwakeChoice, Overview, Session, SettingsView},
    app::{App, shell},
};

pub const COOKIE: &str = "strata_admin";
const COOKIE_MAX_AGE_SECS: u64 = 30 * 24 * 3600;

/// What the UI needs from the proxy. Provided as context to every page and
/// server function call.
pub trait Backend: Send + Sync + 'static {
    fn session(&self, token: Option<&str>) -> Session;
    /// A session token, or a message for the user.
    fn login(&self, password: &str, client: IpAddr) -> Result<String, String>;
    /// A new session token, or a message for the user.
    fn change_password(
        &self,
        token: &str,
        current: &str,
        new: &str,
        client: IpAddr,
    ) -> Result<String, String>;
    /// Ends the session of this token, also for copies of the cookie.
    fn logout(&self, token: &str);
    fn settings(&self) -> SettingsView;
    /// Saves one value; a message for the user if it is invalid.
    fn save_setting(&self, key: &str, value: &str) -> Result<(), String>;
    /// Drops a saved value, so the environment or default applies again.
    fn reset_setting(&self, key: &str) -> Result<(), String>;
    /// The overview now and after every change, for as long as it is held.
    /// While anyone holds one, the proxy checks on the PC and Strata.
    fn overview(&self) -> BoxStream<'static, Overview>;
    /// Starts waking the PC; a message for the user if it cannot be woken.
    fn wake(&self) -> Result<(), String>;
    /// Asks the PC service to sleep; its reason if it refuses.
    fn sleep_now(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Switches gaming mode, waking the PC first if needed.
    fn set_gaming(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>>;
    fn keep_awake(&self, choice: KeepAwakeChoice) -> Result<(), String>;
}

pub type SharedBackend = Arc<dyn Backend>;

pub fn backend() -> SharedBackend {
    expect_context::<SharedBackend>()
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

pub async fn client_ip() -> IpAddr {
    leptos_axum::extract::<ConnectInfo<SocketAddr>>()
        .await
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip())
}

/// The caller's token if logged in, else 401. `allow_must_change` lets a
/// session with the default password through (only for changing it).
pub async fn require_login(allow_must_change: bool) -> Result<String, ServerFnError> {
    let token = token().await;
    match backend().session(token.as_deref()) {
        Session::Ready => Ok(token.unwrap_or_default()),
        Session::MustChange if allow_must_change => Ok(token.unwrap_or_default()),
        Session::MustChange => {
            expect_context::<ResponseOptions>().set_status(StatusCode::FORBIDDEN);
            Err(ServerFnError::new("Bitte zuerst das Passwort ändern."))
        }
        Session::Anonymous => {
            expect_context::<ResponseOptions>().set_status(StatusCode::UNAUTHORIZED);
            Err(ServerFnError::new("Nicht angemeldet."))
        }
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
    if let Ok(value) = HeaderValue::from_str(value) {
        expect_context::<ResponseOptions>().insert_header(header::SET_COOKIE, value);
    }
}

/// The overview as server-sent events, only for a logged-in user.
async fn overview_stream(backend: SharedBackend, headers: HeaderMap) -> Response {
    match backend.session(cookie_token(&headers).as_deref()) {
        Session::Ready => {}
        Session::MustChange => return StatusCode::FORBIDDEN.into_response(),
        Session::Anonymous => return StatusCode::UNAUTHORIZED.into_response(),
    }
    let events = backend.overview().map(|overview| {
        Event::default()
            .json_data(overview)
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

/// Routes for the pages, the server functions and the browser files.
pub fn router<S>(options: LeptosOptions, backend: SharedBackend) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let pkg = PathBuf::from(options.site_root.as_ref()).join(options.site_pkg_dir.as_ref());
    let stream_backend = Arc::clone(&backend);
    let context = move || provide_context(Arc::clone(&backend));
    // Listing the routes renders the app once, which starts the session
    // resource; without the backend in context that call panics.
    let routes =
        generate_route_list_with_exclusions_and_ssg_and_context(App, None, context.clone()).0;
    Router::new()
        .route(
            "/admin/api/overview-stream",
            get(move |headers: HeaderMap| overview_stream(Arc::clone(&stream_backend), headers)),
        )
        .leptos_routes_with_context(&options, routes, context, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .nest_service("/admin/pkg", tower_http::services::ServeDir::new(pkg))
        .with_state(options)
}
