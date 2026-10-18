//! Service on the Strata PC: sleeps on request from the NAS, switches gaming
//! mode and restarts Strata when it stops answering.

pub mod agent;
pub mod catalog;
pub mod command;
pub mod config;
pub mod managed;
pub mod platform;
pub mod strata;

use std::{sync::Arc, time::Duration};

pub use agent::{Agent, Peer, Status};
pub use config::Config;

/// Writes via a temporary file, so a crash never leaves half a file behind.
pub(crate) fn write_replacing(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path))
}

const WATCHDOG_EVERY: Duration = Duration::from_secs(5);

pub async fn run(agent: Arc<Agent>) -> anyhow::Result<()> {
    agent.spawn_watchdog(WATCHDOG_EVERY);
    let listener = tokio::net::TcpListener::bind(agent.cfg.listen).await?;
    tracing::info!("PC-Dienst hört auf {}", agent.cfg.listen);
    axum::serve(
        listener,
        agent::router(agent).into_make_service_with_connect_info::<Peer>(),
    )
    .await?;
    Ok(())
}

/// For the desktop shortcut: switches gaming mode through the running service.
pub async fn switch_gaming(cfg: &Config, want: Option<bool>) -> anyhow::Result<Status> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let base = cfg.local_url();
    let enabled = match want {
        Some(v) => v,
        None => {
            let current: Status = http
                .get(format!("{base}/status"))
                .bearer_auth(&cfg.token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            !current.gaming_mode
        }
    };
    Ok(http
        .post(format!("{base}/gaming"))
        .bearer_auth(&cfg.token)
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
