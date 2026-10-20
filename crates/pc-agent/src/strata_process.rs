//! Starting and stopping Strata. Every start and stop goes through here:
//! jobs, the watchdog, the config check at the service's start, gaming mode
//! and standby. So the pause between stop and start and the check for
//! gaming mode live in one place, and so does the time of the last start,
//! which says whether Strata may still be loading.
//!
//! The watchdog decides *when* Strata needs a restart, a job *what* to
//! change before one; *how* is this module's.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    agent::Agent,
    job::{JobContext, KEPT_LINES, POLL, Step},
};

/// When Strata was last started; the agent keeps it for this module.
#[derive(Default)]
pub(crate) struct LastStart(Mutex<Option<Instant>>);

/// What became of a config change after [`StrataProcess::restart_or_roll_back`].
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Strata came up with it.
    Kept,
    /// Strata did not come up; the old config applies again.
    RolledBack,
}

/// Strata's process on this PC, as the agent starts and stops it.
pub struct StrataProcess<'a> {
    agent: &'a Agent,
}

impl<'a> StrataProcess<'a> {
    pub(crate) fn of(agent: &'a Agent) -> Self {
        Self { agent }
    }

    /// Starts Strata with the managed start script, if one is set, and
    /// follows its output for the Strata-Ausgabe.
    pub fn start(&self) {
        let agent = self.agent;
        let managed = agent.managed();
        let Some((program, args)) = managed.start(&agent.cfg) else {
            tracing::info!("Kein Startskript für Strata eingetragen, Strata bleibt aus");
            return;
        };
        *agent.last_start.0.lock().unwrap() = Some(Instant::now());
        tracing::info!("Strata wird gestartet");
        let mut run = match agent.platform().start_strata(program, args) {
            Ok(run) => run,
            Err(e) => {
                tracing::error!("Strata-Start fehlgeschlagen: {e}");
                return;
            }
        };
        let output = Arc::clone(&agent.output);
        let run_id = output.started();
        // The page shows this output; the key must not reach it. A new key
        // comes with a restart, so the one of this start is enough.
        let key = managed
            .api_key(&agent.cfg)
            .filter(|k| !k.is_empty())
            .map(String::from);
        tokio::spawn(async move {
            while let Some(line) = run.next_output().await {
                match &key {
                    Some(key) => output.push(run_id, &line.replace(key.as_str(), "***")),
                    None => output.push(run_id, &line),
                }
            }
            output.ended(run_id);
        });
    }

    /// Ends the programs the managed values name as Strata.
    pub fn stop(&self) {
        let agent = self.agent;
        tracing::info!("Strata wird beendet");
        let managed = agent.managed();
        let processes = managed.processes(&agent.cfg);
        if processes.is_empty() {
            tracing::info!("Keine Strata-Programme eingetragen, nichts zu beenden");
            return;
        }
        if let Err(e) = agent.platform().stop_strata(processes) {
            tracing::error!("Strata-Ende fehlgeschlagen: {e}");
        }
    }

    /// Strata was started within `within`.
    pub fn started_within(&self, within: Duration) -> bool {
        self.agent
            .last_start
            .0
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < within)
    }

    /// Strata was started so recently that it may still be loading.
    pub fn loading(&self) -> bool {
        self.started_within(Duration::from_secs(self.agent.cfg.load_time_secs))
    }

    /// After a [`stop`](Self::stop): gives the ended Strata time to free its
    /// port, then starts it unless gaming mode is on.
    pub async fn start_again(&self) {
        tokio::time::sleep(self.agent.cfg.stop_settle()).await;
        if !self.agent.gaming() {
            self.start();
        }
    }

    /// Stops Strata and starts it again unless gaming mode is on.
    pub async fn restart(&self) {
        self.stop();
        self.start_again().await;
    }

    /// Waits until Strata has no request at work, so a restart cuts none
    /// off; ends on the job's cancel.
    pub async fn wait_idle(&self, job: &JobContext) -> Result<(), Step> {
        let agent = self.agent;
        job.wait_for(
            "Waiting until Strata finishes the running request",
            None,
            || async { !agent.strata_busy().await },
        )
        .await
    }

    /// Restarts Strata once it has no request at work.
    pub async fn restart_when_idle(&self, job: &JobContext) -> Result<(), Step> {
        self.wait_idle(job).await?;
        job.step("Restarting Strata");
        self.restart().await;
        Ok(())
    }

    /// Restarts Strata once it is idle, after a job changed its config.
    /// If Strata does not answer within `context_start_secs`, or ends
    /// before, `restore` puts the old config back with `restore_step`
    /// shown and Strata starts with it; the job keeps Strata's last lines
    /// then. After a cancel, while Strata is busy or while it comes up,
    /// the old config comes back too and the job ends cancelled.
    pub async fn restart_or_roll_back<R, RF>(
        &self,
        job: &JobContext,
        restore_step: &str,
        restore: R,
    ) -> Result<Applied, Step>
    where
        R: FnOnce() -> RF,
        RF: Future<Output = Result<(), Step>>,
    {
        if let Err(stopped) = self.restart_when_idle(job).await {
            // Strata still runs the old config; only the file goes back.
            job.cleanup(restore()).await?;
            return Err(stopped);
        }
        let started = if self.agent.gaming() {
            Ok(true)
        } else {
            self.came_up(job).await
        };
        if let Ok(true) = started {
            return Ok(Applied::Kept);
        }
        let output = self.agent.output.last_lines(KEPT_LINES);
        job.cleanup(async {
            job.step(restore_step);
            let restored = restore().await;
            if restored.is_ok() {
                self.restart().await;
            }
            // Strata's own words explain the failure, also next to a
            // failed restore.
            job.show_lines(output);
            restored
        })
        .await?;
        // A cancelled wait also leaves the old config, not an unproven one.
        started?;
        Ok(Applied::RolledBack)
    }

    /// Whether the restarted Strata answers: `false` once it has ended or
    /// not answered within `context_start_secs`.
    async fn came_up(&self, job: &JobContext) -> Result<bool, Step> {
        let agent = self.agent;
        job.step("Waiting until Strata is ready");
        let limit = Duration::from_secs(agent.cfg.context_start_secs);
        let started = Instant::now();
        loop {
            if job.cancelled() {
                return Err(Step::Cancelled);
            }
            if agent.strata_healthy().await {
                return Ok(true);
            }
            if !agent.output.reading() || started.elapsed() >= limit {
                return Ok(false);
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
