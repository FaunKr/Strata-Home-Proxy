//! Service on the Strata PC: sleeps on request from the NAS, switches gaming
//! mode and restarts Strata when it stops answering.

pub mod agent;
pub mod catalog;
pub mod command;
pub mod config;
#[cfg(windows)]
mod d3dkmt;
pub mod hardware;
pub mod install;
pub mod job;
pub mod machine;
pub mod managed;
pub mod output;
pub mod platform;
pub mod releases;
pub mod sampling;
pub mod strata;
mod strata_key;
pub mod strata_process;
pub mod terminal;
pub mod update;

use std::{sync::Arc, time::Duration};

pub use agent::{Agent, Peer};
pub use config::Config;
pub use strata_pc_protocol::PcStatus;

/// The crate version, e.g. `0.2.0`: the proxy offers an update only for a
/// higher one, so a new build without a new version is no update.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Only the length: a second copy of the prefix as its own text would sit
/// in the program next to the marker and could be found first.
const MARKER_PREFIX_LEN: usize = "strata-pc-agent-version:".len();
/// The version as plain text in the program, so the NAS reads it from the
/// .exe it serves without running it (see `nas-proxy/src/pc_setup.rs`).
static VERSION_MARKER: &str = concat!("strata-pc-agent-version:", env!("CARGO_PKG_VERSION"), "\n");

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
/// The old program ends a second after it started the new one; after this
/// many attempts (5 s) it hangs, and the new one ends it.
const BIND_STUCK_AFTER: u32 = 10;

pub async fn run(agent: Arc<Agent>) -> anyhow::Result<()> {
    let listener = bind(agent.cfg.listen, || {
        if let Some(pid) = platform::replaced_pid() {
            tracing::warn!(
                "The program before the update (process {pid}) keeps the port, ending it"
            );
            if let Err(e) = platform::end_replaced(pid) {
                tracing::warn!("Process {pid} could not be ended: {e:#}");
            }
        }
    })
    .await?;
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
    strata::check_catalog(&agent);
    agent.spawn_watchdog(WATCHDOG_EVERY);
    tracing::info!("PC service {VERSION} listening on {}", agent.cfg.listen);
    axum::serve(
        listener,
        agent::router(agent).into_make_service_with_connect_info::<Peer>(),
    )
    .await?;
    Ok(())
}

/// Takes the port; while it is taken, tries again, and calls `stuck` once
/// when the old program should have ended long since.
async fn bind(
    addr: std::net::SocketAddr,
    stuck: impl FnOnce(),
) -> std::io::Result<tokio::net::TcpListener> {
    let mut attempt = 1;
    let mut stuck = Some(stuck);
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < BIND_ATTEMPTS => {
                if attempt == BIND_STUCK_AFTER
                    && let Some(stuck) = stuck.take()
                {
                    stuck();
                }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn a_free_port_is_taken_without_ending_anything() {
        let called = std::cell::Cell::new(false);
        let listener = bind("127.0.0.1:0".parse().unwrap(), || called.set(true))
            .await
            .unwrap();
        assert!(listener.local_addr().is_ok());
        assert!(!called.get());
    }

    #[tokio::test]
    async fn a_port_the_old_program_keeps_is_taken_once_it_is_ended() {
        let old = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = old.local_addr().unwrap();
        let old = Mutex::new(Some(old));
        let listener = bind(addr, || drop(old.lock().unwrap().take()))
            .await
            .unwrap();
        assert_eq!(listener.local_addr().unwrap(), addr);
        assert!(old.lock().unwrap().is_none(), "the old program was ended");
    }
}
