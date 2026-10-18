//! NAS proxy for Strata: wakes the PC on demand and sends it to sleep when idle.

pub mod agent;
pub mod config;
pub mod events;
pub mod proxy;
pub mod settings;
pub mod state;
pub mod wol;

use std::sync::Arc;

use axum::Router;

pub use config::Config;
pub use settings::Settings;
pub use state::AppState;

/// The API router: every path is forwarded to Strata.
pub fn api_router(state: Arc<AppState>) -> Router {
    Router::new().fallback(proxy::handle).with_state(state)
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
