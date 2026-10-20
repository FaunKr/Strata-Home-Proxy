//! Update from the web UI: loads the program the NAS carries, stops Strata,
//! puts the program in place of the running one and hands over to it. The
//! new program starts Strata again when this one stopped a running Strata
//! (see [`take_start_after_update`]).

use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::{agent::Agent, job::POLL};

/// Where the NAS serves the program (see `nas-proxy/src/pc_setup.rs`).
const PROGRAM_PATH: &str = "/admin/agent/strata-pc-agent.exe";
/// The program is a few MB on the home network; this is plenty.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Between the answer to the NAS and ending this process.
pub const EXIT_DELAY: Duration = Duration::from_secs(1);
const DOWNLOAD_FILE: &str = "strata-pc-agent.exe.neu";
/// Left in the state folder when the update stopped a running Strata; the
/// new program starts Strata then.
const MARKER_FILE: &str = "strata-after-update";

/// Why an update did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// Not now, or not with this setup; the program was not touched. Strata
    /// may have been stopped when it would not end ("did not stop").
    Refused(String),
    /// Download or replacing failed; the running program stays.
    Failed(String),
}

/// Loads the new program and puts it in place. On success the new program
/// runs and this process ends shortly after, so the answer still goes out.
pub async fn update(agent: &Arc<Agent>) -> Result<(), UpdateError> {
    let Some(nas) = agent.cfg.nas_url.clone() else {
        return Err(UpdateError::Refused(
            "The PC service does not know the NAS's address. Set it up again once with the install command from the PC area of the web UI."
                .into(),
        ));
    };
    if agent.updating.swap(true, Ordering::SeqCst) {
        return Err(UpdateError::Refused("An update is running already".into()));
    }
    // Set first, so no job starts between this check and the restart.
    if agent.jobs.running() {
        agent.updating.store(false, Ordering::SeqCst);
        return Err(UpdateError::Refused("A Strata job is running".into()));
    }
    let result = hand_over(agent, &nas).await;
    match &result {
        Ok(()) => {
            tracing::info!("Update in place, the PC service restarts");
            let agent = Arc::clone(agent);
            tokio::spawn(async move {
                tokio::time::sleep(EXIT_DELAY).await;
                agent.platform().exit();
            });
        }
        Err(e) => {
            tracing::warn!("Update fehlgeschlagen: {e:?}");
            agent.updating.store(false, Ordering::SeqCst);
        }
    }
    result
}

/// Download, then Strata's stop, then the swap. Strata goes down only once
/// the new program is here; when the swap fails, the Strata this stopped
/// starts again.
async fn hand_over(agent: &Arc<Agent>, nas: &str) -> Result<(), UpdateError> {
    refuse_while_strata_works(agent).await?;
    let bytes = download(nas).await?;
    // The download may take minutes; a request may have begun meanwhile.
    refuse_while_strata_works(agent).await?;
    let was_running = stop_strata(agent).await?;
    let marker = marker(agent);
    if was_running && let Err(e) = std::fs::write(&marker, b"") {
        // The watchdog of the new program starts Strata then, only later.
        tracing::warn!("{} could not be written: {e}", marker.display());
    }
    let replaced = replace(agent, bytes).await;
    if replaced.is_err() {
        let _ = std::fs::remove_file(&marker);
        if was_running {
            // Still under the update flag, so no second update begins
            // before Strata is back.
            tokio::time::sleep(agent.cfg.stop_settle()).await;
            if agent.may_start_strata_itself() {
                agent.strata().start_after_failed_update();
            }
        }
    }
    replaced
}

async fn refuse_while_strata_works(agent: &Agent) -> Result<(), UpdateError> {
    if agent.strata_busy().await {
        return Err(UpdateError::Refused(
            "Strata is processing a request".into(),
        ));
    }
    Ok(())
}

/// Stops Strata, also one started by hand, and waits until it no longer
/// answers. True when it ran before. A running Strata that cannot be
/// stopped, or still answers, refuses the update and is left as it is, so
/// no second Strata starts next to it.
async fn stop_strata(agent: &Arc<Agent>) -> Result<bool, UpdateError> {
    let did_not_stop =
        || UpdateError::Refused("Strata did not stop; the PC service was not updated".into());
    let process = agent.strata();
    let was_running = process.loading() || agent.strata_healthy().await;
    if !process.stop() && was_running {
        tracing::warn!("Strata could not be stopped, no update");
        return Err(did_not_stop());
    }
    let limit = tokio::time::Instant::now() + agent.cfg.update_stop_wait();
    while agent.strata_healthy().await {
        if tokio::time::Instant::now() >= limit {
            tracing::warn!("Strata still answers after the stop, no update");
            return Err(did_not_stop());
        }
        tokio::time::sleep(POLL).await;
    }
    Ok(was_running)
}

fn marker(agent: &Agent) -> PathBuf {
    agent.cfg.state_dir().join(MARKER_FILE)
}

/// At the service's start: whether the program before stopped a running
/// Strata for its update, so this one starts Strata. Removes the marker, so
/// it counts once.
pub fn take_start_after_update(agent: &Agent) -> bool {
    std::fs::remove_file(marker(agent)).is_ok()
}

async fn download(nas: &str) -> Result<Vec<u8>, UpdateError> {
    let failed = |text: String| UpdateError::Failed(text);
    let url = format!("{}{PROGRAM_PATH}", nas.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| failed(format!("Download from the NAS failed ({url}): {e}")))?;
    if !resp.status().is_success() {
        return Err(failed(format!(
            "The NAS sends no program (HTTP {})",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| failed(format!("Download from the NAS was interrupted: {e}")))?;
    if !bytes.starts_with(b"MZ") {
        return Err(failed("The NAS sent no Windows program".into()));
    }
    Ok(bytes.to_vec())
}

async fn replace(agent: &Arc<Agent>, bytes: Vec<u8>) -> Result<(), UpdateError> {
    let file = agent.cfg.state_dir().join(DOWNLOAD_FILE);
    let platform_agent = Arc::clone(agent);
    let downloaded = file.clone();
    // A few MB to disk, then the swap: off the async workers, so `/status`
    // keeps answering.
    let replaced = tokio::task::spawn_blocking(move || {
        std::fs::write(&downloaded, &bytes)
            .map_err(|e| anyhow::anyhow!("{} cannot be saved: {e}", downloaded.display()))?;
        platform_agent.platform().replace_program(&downloaded)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    if let Err(e) = replaced {
        let _ = std::fs::remove_file(&file);
        return Err(UpdateError::Failed(format!(
            "New program not put in place: {e:#}"
        )));
    }
    Ok(())
}
