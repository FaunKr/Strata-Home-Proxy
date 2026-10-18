//! NAS proxy for Strata: wakes the PC on demand and sends it to sleep when idle.

pub mod agent;
pub mod config;
pub mod events;
pub mod proxy;
pub mod settings;
pub mod state;
pub mod wol;

use std::sync::Arc;

use axum::{Router, http::StatusCode, routing::any};

pub use config::Config;
pub use settings::Settings;
pub use state::AppState;

/// The router on the API port: the web UI under `/admin/`, every other path
/// is forwarded to Strata. UI calls never wake the PC or count as activity.
pub fn api_router(state: Arc<AppState>) -> Router {
    let options = strata_admin_ui::server::options(strata_admin_ui::server::site_root());
    let admin = strata_admin_ui::server::router(options, || {});
    Router::new()
        .merge(admin)
        // Unknown UI paths must not reach Strata either.
        .route("/admin/{*rest}", any(admin_not_found))
        .fallback(proxy::handle)
        .with_state(state)
}

async fn admin_not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "Nicht gefunden")
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let state = AppState::with_settings(Settings::new(cfg, &Config::env_keys()));
    let current = state.cfg();
    let listen = current.listen_addr;
    if current.strata_url.is_none() {
        tracing::warn!("STRATA_URL fehlt: der Proxy ist nicht eingerichtet");
    }
    if current.pc_mac.is_none() {
        tracing::warn!("PC_MAC fehlt: der PC kann nicht geweckt werden");
    }
    if current.pc_agent_url.is_none() {
        tracing::warn!("PC_AGENT_URL fehlt: kein Gaming-Modus und kein automatisches Schlafen");
    }
    state.spawn_idle_loop();
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!("API-Proxy hört auf {listen}");
    axum::serve(listener, api_router(state)).await?;
    Ok(())
}
