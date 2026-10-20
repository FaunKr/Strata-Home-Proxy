//! Service on the Strata PC: sleeps on request from the NAS, switches gaming
//! mode and restarts Strata when it stops answering.

pub mod agent;
pub mod catalog;
pub mod command;
pub mod config;
pub mod hardware;
pub mod install;
pub mod job;
pub mod managed;
pub mod output;
pub mod platform;
pub mod sampling;
pub mod strata;
pub mod strata_process;
pub mod update;

use std::{sync::Arc, time::Duration};

pub use agent::{Agent, Peer};
pub use config::Config;
pub use strata_pc_protocol::PcStatus;

/// Crate version plus build, e.g. `0.1.0+558db97`; see `build.rs`.
pub const VERSION: &str = env!("STRATA_PC_AGENT_VERSION");

/// Only the length: a second copy of the prefix as its own text would sit
/// in the program next to the marker and could be found first.
const MARKER_PREFIX_LEN: usize = "strata-pc-agent-version:".len();
/// The version as plain text in the program, so the NAS reads it from the
/// .exe it serves without running it (see `nas-proxy/src/pc_setup.rs`).
static VERSION_MARKER: &str = concat!(
    "strata-pc-agent-version:",
    env!("STRATA_PC_AGENT_VERSION"),
    "\n"
);

/// The version as `/status` reports it. Read from the marker at run time,
/// so no linker drops the marker from the program (`#[used]` alone does not
/// keep it with MSVC).
pub fn version() -> &'static str {
    let marker: &'static str = std::hint::black_box(VERSION_MARKER);
    marker
        .get(MARKER_PREFIX_LEN..)
        .and_then(|v| v.strip_suffix('\n'))
        .unwrap_or(VERSION)
}

/// Writes via a temporary file, so a crash never leaves half a file behind.
pub(crate) fn write_replacing(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path))
}

const WATCHDOG_EVERY: Duration = Duration::from_secs(5);
/// After an update the new program starts while the old one still answers;
/// it waits this long in all for the port.
const BIND_ATTEMPTS: u32 = 60;
const BIND_PAUSE: Duration = Duration::from_millis(500);

pub async fn run(agent: Arc<Agent>) -> anyhow::Result<()> {
    let listener = bind(agent.cfg.listen).await?;
    // The old program has ended once the port is free; Windows may keep its
    // file a moment longer.
    tokio::spawn(async {
        for _ in 0..10 {
            if tokio::task::spawn_blocking(platform::remove_replaced_program)
                .await
                .unwrap_or(false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    // Before the first job can change a config too.
    tokio::spawn(strata::manage_configs_at_start(&agent).await);
    agent.spawn_watchdog(WATCHDOG_EVERY);
    tracing::info!("PC-Dienst {VERSION} hört auf {}", agent.cfg.listen);
    axum::serve(
        listener,
        agent::router(agent).into_make_service_with_connect_info::<Peer>(),
    )
    .await?;
    Ok(())
}

async fn bind(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let mut attempt = 1;
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < BIND_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(BIND_PAUSE).await;
            }
            other => return other,
        }
    }
}

/// For the desktop shortcut: switches gaming mode through the running service.
pub async fn switch_gaming(cfg: &Config, want: Option<bool>) -> anyhow::Result<PcStatus> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let base = cfg.local_url();
    let enabled = match want {
        Some(v) => v,
        None => {
            let current: PcStatus = http
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
        .json(&strata_pc_protocol::GamingRequest { enabled })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
