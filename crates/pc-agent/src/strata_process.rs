//! Starting and stopping Strata. Every start and stop goes through here:
//! jobs, the watchdog, the config check at the service's start, gaming mode
//! and standby. So the pause between stop and start and the check for
//! gaming mode live in one place, and so does the time of the last start,
//! which says whether Strata may still be loading.
//!
//! The watchdog decides *when* Strata needs a restart, a job *what* to
//! change before one; *how* is this module's.
//!
//! Restarts may overlap, e.g. the watchdog's with gaming mode switched off
//! during its pause: a start after a pause is left out when Strata was
//! started since the last stop, so only one Strata runs.

use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    agent::Agent,
    job::{JobContext, KEPT_LINES, Step},
};

/// When Strata was last started and stopped; the agent keeps it for this
/// module.
#[derive(Default)]
pub(crate) struct Times(Mutex<Seen>);

#[derive(Default)]
struct Seen {
    start: Option<Instant>,
    stop: Option<Instant>,
}

impl Seen {
    /// Strata was started after the last stop.
    fn started_since_stop(&self) -> bool {
        match (self.start, self.stop) {
            (Some(start), Some(stop)) => start > stop,
            (start, None) => start.is_some(),
            (None, Some(_)) => false,
        }
    }

    /// Strata was started within `within` and not stopped since.
    fn loading(&self, within: Duration) -> bool {
        self.started_since_stop() && self.start.is_some_and(|t| t.elapsed() < within)
    }
}

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
    /// follows its output for the Strata output.
    pub fn start(&self) {
        self.start_if(|_| true);
    }

    /// Starts Strata unless the service started it since the last stop.
    /// True when Strata runs from a start since the last stop, this one or
    /// the other; false without a start script or when the start failed.
    fn start_once(&self) -> bool {
        let mut other = false;
        let started = self.start_if(|seen| {
            let started = seen.started_since_stop();
            if started {
                tracing::info!("Strata was started in the meantime, no second start");
            }
            other = started;
            !started
        });
        started || other
    }

    /// Starts Strata if `wanted` holds for what was seen; checked and
    /// recorded at once, so two starts cannot both pass. True when this
    /// call started Strata.
    fn start_if(&self, wanted: impl FnOnce(&Seen) -> bool) -> bool {
        let agent = self.agent;
        let managed = agent.managed();
        let Some((program, args)) = managed.start(&agent.cfg) else {
            tracing::info!("No start script set for Strata, Strata stays off");
            return false;
        };
        let (before, now) = {
            let mut seen = agent.times.0.lock().unwrap();
            if !wanted(&seen) {
                return false;
            }
            let now = Instant::now();
            (seen.start.replace(now), now)
        };
        tracing::info!("Starting Strata");
        let mut run = match agent.platform().start_strata(program, args) {
            Ok(run) => run,
            Err(e) => {
                tracing::error!("Starting Strata failed: {e}");
                // No Strata loads; the start before counts again.
                let mut seen = agent.times.0.lock().unwrap();
                if seen.start == Some(now) {
                    seen.start = before;
                }
                return false;
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
        true
    }

    /// Ends the programs the managed values name as Strata.
    pub fn stop(&self) {
        let agent = self.agent;
        agent.times.0.lock().unwrap().stop = Some(Instant::now());
        tracing::info!("Stopping Strata");
        let managed = agent.managed();
        let processes = managed.processes(&agent.cfg);
        if processes.is_empty() {
            tracing::info!("No Strata programs set, nothing to stop");
            return;
        }
        if let Err(e) = agent.platform().stop_strata(processes) {
            tracing::error!("Stopping Strata failed: {e}");
        }
    }

    /// Strata was started within `within`.
    pub fn started_within(&self, within: Duration) -> bool {
        self.agent
            .times
            .0
            .lock()
            .unwrap()
            .start
            .is_some_and(|t| t.elapsed() < within)
    }

    /// Strata was started so recently that it may still be loading, and
    /// not stopped since.
    pub fn loading(&self) -> bool {
        let within = Duration::from_secs(self.agent.cfg.load_time_secs);
        self.agent.times.0.lock().unwrap().loading(within)
    }

    /// After a [`stop`](Self::stop): gives the ended Strata time to free its
    /// port, then starts it unless gaming mode is on or it was started
    /// meanwhile.
    pub async fn start_again(&self) {
        tokio::time::sleep(self.agent.cfg.stop_settle()).await;
        if !self.agent.gaming() {
            self.start_once();
        }
    }

    /// The pause after a [`stop`](Self::stop) outside a job, then a start
    /// unless gaming mode is on, auto-start is off or a job began meanwhile
    /// (it handles Strata then). `check` comes after the pause; its error
    /// leaves Strata stopped. `Ok(true)` when Strata runs from a start
    /// since the stop.
    pub async fn start_after_stop<E>(
        &self,
        check: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        tokio::time::sleep(self.agent.cfg.stop_settle()).await;
        check()?;
        if self.agent.jobs.running() {
            tracing::info!("A Strata job has begun; it starts Strata itself");
            return Ok(false);
        }
        if self.agent.gaming() || !self.agent.cfg.auto_start {
            return Ok(false);
        }
        Ok(self.start_once())
    }

    /// Stops Strata and starts it again unless gaming mode is on.
    pub async fn restart(&self) {
        self.stop();
        self.start_again().await;
    }

    /// A restart outside a job, by the watchdog or the config check at the
    /// service's start: none while a job runs, and no start after the
    /// pause if a job began meanwhile, as the job handles Strata then.
    pub async fn restart_unless_job_runs(&self) {
        if self.agent.jobs.running() {
            return;
        }
        self.stop();
        tokio::time::sleep(self.agent.cfg.stop_settle()).await;
        if self.agent.jobs.running() {
            tracing::info!("A Strata job has begun, it starts Strata itself");
            return;
        }
        if !self.agent.gaming() {
            self.start_once();
        }
    }

    /// Restarts Strata for `job`, with a step that says what happens: in
    /// gaming mode Strata only stops and starts with the change later.
    pub async fn restart_for(&self, job: &JobContext) {
        job.step(if self.agent.gaming() {
            "Stopping Strata; it starts with the change when gaming mode ends"
        } else {
            "Restarting Strata"
        });
        self.restart().await;
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
        self.restart_for(job).await;
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
        let limit = Duration::from_secs(agent.cfg.context_start_secs);
        let answer = job
            .wait_until("Waiting until Strata is ready", Some(limit), || async {
                if agent.strata_healthy().await {
                    Some(true)
                } else if !agent.output.reading() {
                    Some(false)
                } else {
                    None
                }
            })
            .await?;
        Ok(answer.unwrap_or(false))
    }
}
