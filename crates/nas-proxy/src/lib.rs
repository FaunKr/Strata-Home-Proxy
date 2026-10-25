//! The proxy for Strata on the NAS: wakes the PC on demand and sends it to sleep when idle.

pub mod admin;
pub mod agent;
pub mod api_keys;
pub mod auth;
pub mod clock;
pub mod code_bench;
pub mod config;
pub mod events;
pub mod files;
pub mod forced;
pub mod history;
pub mod lifecycle;
pub mod models;
pub mod optimization_prompt;
pub mod overview;
pub mod pc_changelog;
pub mod pc_setup;
pub mod plug;
pub mod proxy;
pub mod public_name;
pub mod request_log;
pub mod request_meter;
pub mod settings;
pub mod speed_history;
pub mod state;
pub mod strata;
pub mod strata_client;
pub mod strata_key;
pub mod strata_output;
pub mod timeouts;
pub mod update_summary;
pub mod wol;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub use config::Config;
pub use settings::Settings;
pub use state::AppState;

/// The router on the API port: the web UI under `/admin/`, every other path
/// is forwarded to Strata. UI calls never wake the PC or count as activity.
pub fn api_router(state: Arc<AppState>) -> Router {
    let options = strata_admin_ui::server::options(strata_admin_ui::server::site_root());
    let admin = strata_admin_ui::server::router(options, admin::backend(&state));
    Router::new()
        .merge(admin)
        .merge(pc_setup::router())
        .fallback(fallback)
        .with_state(state)
}

/// Everything the UI does not answer goes to Strata, except unknown UI paths.
async fn fallback(state: State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path();
    if path == "/admin" || path.starts_with("/admin/") {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    proxy::handle(state, req).await
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let state = AppState::with_settings(Settings::new(cfg, &Config::env_keys()));
    let current = state.cfg();
    let listen = current.listen_addr;
    if current.strata_url.is_none() {
        tracing::warn!("STRATA_URL is missing: the proxy is not set up");
    }
    if current.pc_mac.is_none() {
        tracing::warn!("PC_MAC is missing: the PC cannot be woken");
    }
    if current.pc_agent_url.is_none() {
        tracing::warn!("PC_AGENT_URL is missing: no gaming mode and no automatic sleep");
    }
    state.spawn_idle_loop();
    state.spawn_plug_loop();
    state.spawn_beat_loop();
    code_bench::resume(&state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!("API proxy listening on {listen}");
    // The client address is needed to slow down password guessing.
    let app = api_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app).await?;
    Ok(())
}
