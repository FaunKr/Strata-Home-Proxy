//! Update from the web UI: loads the program the NAS carries, puts it in
//! place of the running one and hands over to it.

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::agent::Agent;

/// Where the NAS serves the program (see `nas-proxy/src/pc_setup.rs`).
const PROGRAM_PATH: &str = "/admin/agent/strata-pc-agent.exe";
/// The program is a few MB on the home network; this is plenty.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Between the answer to the NAS and ending this process.
pub const EXIT_DELAY: Duration = Duration::from_secs(1);
const DOWNLOAD_FILE: &str = "strata-pc-agent.exe.neu";

/// Why an update did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// Not now, or not with this setup; nothing was touched.
    Refused(String),
    /// Download or replacing failed; the running program stays.
    Failed(String),
}

/// Loads the new program and puts it in place. On success the new program
/// runs and this process ends shortly after, so the answer still goes out.
pub async fn update(agent: &Arc<Agent>) -> Result<(), UpdateError> {
    let Some(nas) = agent.cfg.nas_url.clone() else {
        return Err(UpdateError::Refused(
            "Der PC-Dienst kennt die Adresse des NAS nicht. Einmal mit dem Befehl aus dem Bereich PC der Weboberfläche neu einrichten."
                .into(),
        ));
    };
    if agent.updating.swap(true, Ordering::SeqCst) {
        return Err(UpdateError::Refused("Ein Update läuft schon".into()));
    }
    // Set first, so no job starts between this check and the restart.
    // A restart of the service can take Strata down with it (its output runs
    // through the service); Strata then restarts, which only hurts mid-work.
    let busy = if agent.jobs.running() {
        Some("A Strata job is running")
    } else if agent.strata_busy().await {
        Some("Strata is processing a request")
    } else {
        None
    };
    if let Some(reason) = busy {
        agent.updating.store(false, Ordering::SeqCst);
        return Err(UpdateError::Refused(reason.into()));
    }
    let result = download_and_replace(agent, &nas).await;
    match &result {
        Ok(()) => {
            tracing::info!("Update eingesetzt, der PC-Dienst startet neu");
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

async fn download_and_replace(agent: &Arc<Agent>, nas: &str) -> Result<(), UpdateError> {
    let failed = |text: String| UpdateError::Failed(text);
    let url = format!("{}{PROGRAM_PATH}", nas.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| failed(format!("Download vom NAS fehlgeschlagen ({url}): {e}")))?;
    if !resp.status().is_success() {
        return Err(failed(format!(
            "Der NAS liefert kein Programm (HTTP {})",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| failed(format!("Download vom NAS abgebrochen: {e}")))?;
    if !bytes.starts_with(b"MZ") {
        return Err(failed("Der NAS lieferte kein Windows-Programm".into()));
    }
    let file = agent.cfg.state_dir().join(DOWNLOAD_FILE);
    let platform_agent = Arc::clone(agent);
    let downloaded = file.clone();
    // A few MB to disk, then the swap: off the async workers, so `/status`
    // keeps answering.
    let replaced = tokio::task::spawn_blocking(move || {
        std::fs::write(&downloaded, &bytes)
            .map_err(|e| anyhow::anyhow!("{} nicht speicherbar: {e}", downloaded.display()))?;
        platform_agent.platform().replace_program(&downloaded)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    if let Err(e) = replaced {
        let _ = std::fs::remove_file(&file);
        return Err(failed(format!("Neues Programm nicht eingesetzt: {e:#}")));
    }
    Ok(())
}
