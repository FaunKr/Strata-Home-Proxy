//! The running Strata job and the context each job works through.
//!
//! A job script (see [`crate::strata`]) never touches the job's state
//! itself: it shows its steps, learns of a cancel, waits, runs commands and
//! cleans up through its [`JobContext`]. Only this module and the HTTP
//! handlers (status, current job, cancel) see the state, through [`Jobs`].

use std::{
    collections::VecDeque,
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use strata_pc_protocol::{StrataJob, StrataJobKind, StrataJobState};

use crate::{
    agent::Agent,
    command::{Cancel, CommandSpec},
};

const JOB_FILE: &str = "strata-job.json";
/// How often a job looks again while it waits.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// Lines of output a job keeps, for the page to show after a failure.
pub(crate) const KEPT_LINES: usize = 20;

/// Why a job stopped early.
#[derive(Debug)]
pub enum Step {
    Cancelled,
    Failed(String),
}

impl From<String> for Step {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

/// The last job, and what the running one needs besides it.
#[derive(Default)]
pub struct Jobs {
    inner: Mutex<JobsInner>,
}

#[derive(Default)]
struct JobsInner {
    current: Option<StrataJob>,
    /// Ends the command the running job waits for.
    cancel: Option<Cancel>,
    cancelled: bool,
    progress: Progress,
    /// The key on the running command's line, kept out of its stored output.
    secret: Option<String>,
    /// The last command's benchmark speeds, read as its output comes.
    bench: BenchReading,
    /// Goes before each step, e.g. which model of a comparison.
    step_prefix: Option<String>,
}

impl JobsInner {
    fn running(&self) -> Option<&StrataJob> {
        self.current
            .as_ref()
            .filter(|j| j.state == StrataJobState::Running)
    }
}

impl Jobs {
    /// The last job from `dir`; one that was running then is interrupted now.
    pub fn load(dir: &Path) -> Self {
        let current = std::fs::read_to_string(dir.join(JOB_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<StrataJob>(&text).ok())
            .map(|mut job| {
                if job.state == StrataJobState::Running {
                    job.state = StrataJobState::Interrupted;
                    job.ended = Some(unix_now());
                }
                job
            });
        Self {
            inner: Mutex::new(JobsInner {
                current,
                ..JobsInner::default()
            }),
        }
    }

    pub fn current(&self) -> Option<StrataJob> {
        self.inner.lock().unwrap().current.clone()
    }

    pub fn running(&self) -> bool {
        self.inner.lock().unwrap().running().is_some()
    }

    /// Ends the running job; downloaded parts stay for the next start.
    pub fn cancel(&self) -> bool {
        let cancel = {
            let mut inner = self.inner.lock().unwrap();
            if inner.running().is_none() {
                return false;
            }
            inner.cancelled = true;
            inner.cancel.clone()
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        true
    }
}

/// What a job works through; one per job.
pub struct JobContext {
    agent: Arc<Agent>,
}

impl JobContext {
    /// Records a new running job of `kind`, or gives the kind of the one
    /// that runs already.
    pub(crate) fn begin(
        agent: &Arc<Agent>,
        kind: StrataJobKind,
        model: Option<String>,
    ) -> Result<(Self, StrataJob), StrataJobKind> {
        let job = {
            let mut inner = agent.jobs.inner.lock().unwrap();
            if let Some(running) = inner.running() {
                return Err(running.kind);
            }
            let id = inner
                .current
                .as_ref()
                .and_then(|j| j.id)
                .map_or(1, |id| id + 1);
            let job = StrataJob {
                id: Some(id),
                kind,
                model,
                state: StrataJobState::Running,
                step: None,
                percent: None,
                done_bytes: None,
                total_bytes: None,
                bytes_per_sec: None,
                eta_secs: None,
                lines: Vec::new(),
                error: None,
                result: None,
                started: unix_now(),
                ended: None,
            };
            *inner = JobsInner {
                current: Some(job.clone()),
                ..JobsInner::default()
            };
            job
        };
        let context = Self {
            agent: Arc::clone(agent),
        };
        context.save();
        Ok((context, job))
    }

    pub fn agent(&self) -> &Arc<Agent> {
        &self.agent
    }

    fn with<T>(&self, f: impl FnOnce(&mut JobsInner) -> T) -> T {
        f(&mut self.agent.jobs.inner.lock().unwrap())
    }

    /// Shows `step`, after the prefix if one is set.
    pub fn step(&self, step: &str) {
        self.with(|inner| {
            let step = match &inner.step_prefix {
                Some(prefix) => format!("{prefix} · {step}"),
                None => step.to_string(),
            };
            if let Some(job) = inner.current.as_mut() {
                job.step = Some(step);
                job.percent = None;
            }
        });
        self.save();
    }

    /// Sets what goes before each step from now on, or none.
    pub fn set_prefix(&self, prefix: Option<String>) {
        self.with(|inner| inner.step_prefix = prefix);
    }

    pub fn cancelled(&self) -> bool {
        self.with(|inner| inner.cancelled)
    }

    /// Waits until `ready` holds, showing `step` while it does not; ends on
    /// cancel and fails after `limit`.
    pub async fn wait_for<F, Fut>(
        &self,
        step: &str,
        limit: Option<Duration>,
        mut ready: F,
    ) -> Result<(), Step>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        let started = Instant::now();
        match self
            .wait_until(step, limit, || {
                let ready = ready();
                async { ready.await.then_some(()) }
            })
            .await?
        {
            Some(()) => Ok(()),
            None => Err(format!(
                "{step}: gave up after {} min",
                started.elapsed().as_secs() / 60
            )
            .into()),
        }
    }

    /// Waits until `answer` gives a value, showing `step` while it does not;
    /// ends on cancel, and with `None` after `limit`.
    pub async fn wait_until<T, F, Fut>(
        &self,
        step: &str,
        limit: Option<Duration>,
        mut answer: F,
    ) -> Result<Option<T>, Step>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let started = Instant::now();
        let mut shown = false;
        loop {
            if self.cancelled() {
                return Err(Step::Cancelled);
            }
            if let Some(value) = answer().await {
                return Ok(Some(value));
            }
            if !shown {
                self.step(step);
                shown = true;
            }
            if limit.is_some_and(|l| started.elapsed() >= l) {
                return Ok(None);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Keeps `secret` out of the output of the commands that follow.
    pub(crate) fn hide(&self, secret: &str) {
        self.with(|inner| inner.secret = Some(secret.to_string()));
    }

    /// The speeds the last command printed.
    pub(crate) fn bench(&self) -> BenchReading {
        self.with(|inner| inner.bench)
    }

    /// Shows `lines` as the job's output.
    pub fn show_lines(&self, lines: Vec<String>) {
        self.with(|inner| {
            if let Some(job) = inner.current.as_mut() {
                job.lines = lines;
            }
        });
    }

    /// Runs `cleanup` also after a cancel: the job's cancel is spent then,
    /// only a new one ends the cleanup. The job counts as cancelled after
    /// it if it was before.
    pub async fn cleanup<T>(&self, cleanup: impl Future<Output = T>) -> T {
        let was = self.with(|inner| std::mem::take(&mut inner.cancelled));
        let result = cleanup.await;
        self.with(|inner| inner.cancelled |= was);
        result
    }

    /// Runs one command of the job: keeps its output, reads setup's progress
    /// when `progress` is set and the benchmark's speeds, and ends it on
    /// cancel.
    pub(crate) async fn command(&self, spec: CommandSpec, progress: bool) -> Result<(), Step> {
        let mut run = self
            .agent
            .platform()
            .run(&spec)
            .map_err(|e| Step::Failed(e.to_string()))?;
        let cancelled = self.with(|inner| {
            inner.bench = BenchReading::default();
            if !inner.cancelled {
                inner.cancel = Some(run.canceller());
            }
            inner.cancelled
        });
        if cancelled {
            run.canceller()();
            return Err(Step::Cancelled);
        }
        while let Some(line) = run.next_line().await {
            let step_changed = self.with(|inner| {
                let JobsInner {
                    current,
                    progress: p,
                    secret,
                    bench,
                    step_prefix,
                    ..
                } = inner;
                let line = match secret.as_deref() {
                    Some(key) => line.replace(key, "***"),
                    None => line,
                };
                bench.read(&line);
                let job = current.as_mut()?;
                let before = job.step.clone();
                if progress {
                    p.read(&line, job);
                    // In a comparison: which model the step belongs to.
                    if job.step != before
                        && let (Some(prefix), Some(step)) =
                            (step_prefix.as_deref(), job.step.as_mut())
                    {
                        *step = format!("{prefix} · {step}");
                    }
                }
                let mut lines: VecDeque<String> = std::mem::take(&mut job.lines).into();
                lines.push_back(line);
                while lines.len() > KEPT_LINES {
                    lines.pop_front();
                }
                job.lines = lines.into();
                Some(job.step != before)
            });
            if step_changed == Some(true) {
                self.save();
            }
        }
        let code = run.wait().await;
        let (cancelled, error) = self.with(|inner| {
            inner.cancel = None;
            (
                inner.cancelled,
                inner.current.as_ref().and_then(|j| j.error.clone()),
            )
        });
        if cancelled {
            return Err(Step::Cancelled);
        }
        match code {
            Some(0) => Ok(()),
            code => {
                let program = spec.program.file_name().map_or_else(
                    || spec.program.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let ended = match code {
                    Some(code) => format!("{program} ended with code {code}"),
                    None => format!("{program} was ended"),
                };
                Err(Step::Failed(match error {
                    Some(error) => format!("{ended}: {error}"),
                    None => ended,
                }))
            }
        }
    }

    /// Ends the job with what it gave.
    pub(crate) fn finish(self, result: Result<Option<String>, Step>) {
        self.with(|inner| {
            let Some(job) = inner.current.as_mut() else {
                return;
            };
            job.ended = Some(unix_now());
            job.bytes_per_sec = None;
            job.eta_secs = None;
            match result {
                Ok(result) => {
                    job.state = StrataJobState::Done;
                    job.result = result;
                    job.percent = Some(100);
                }
                Err(Step::Failed(error)) => {
                    job.state = StrataJobState::Failed;
                    job.error = Some(error);
                }
                Err(Step::Cancelled) => job.state = StrataJobState::Cancelled,
            }
        });
        self.save();
        if let Some(job) = self.agent.jobs.current() {
            tracing::info!(kind = ?job.kind, state = ?job.state, error = ?job.error, "Strata job finished");
        }
    }

    fn save(&self) {
        let Some(job) = self.agent.jobs.current() else {
            return;
        };
        let text = serde_json::to_string_pretty(&job).expect("job serializes");
        if let Err(e) = crate::write_replacing(&self.agent.cfg.state_dir().join(JOB_FILE), &text) {
            tracing::warn!("{JOB_FILE} not saved: {e}");
        }
    }
}

/// For tests of what job scripts build on.
#[doc(hidden)]
pub mod testing {
    use std::sync::Arc;

    use strata_pc_protocol::StrataJobKind;

    use super::JobContext;
    use crate::agent::Agent;

    /// A running job, as a job script gets it; `POST /strata/job/cancel`
    /// cancels it.
    pub fn job(agent: &Arc<Agent>) -> JobContext {
        JobContext::begin(agent, StrataJobKind::SetContext, None)
            .map(|(context, _)| context)
            .expect("no job is running")
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The speeds a benchmark printed so far.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BenchReading {
    pub decode: Option<f64>,
    pub prompt: Option<f64>,
}

impl BenchReading {
    /// From the short table `tools/strata_bench.py` prints at the end.
    fn read(&mut self, line: &str) {
        if let Some(tps) = table_value(line, "chat decode tok/s") {
            self.decode = Some(tps);
        } else if let Some(tps) = table_value(line, "4K prompt tok/s") {
            self.prompt = Some(tps);
        }
    }
}

/// `  chat decode tok/s     38.50   (37.10 - 39.20, n=3)` → 38.5 for
/// `chat decode tok/s`.
fn table_value(line: &str, name: &str) -> Option<f64> {
    line.trim()
        .strip_prefix(name)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Reads setup's output: the step, a download's progress, an error. The
/// same lines Strata's own MCP tool reads (`tools/strata_mcp.py`).
#[derive(Default)]
struct Progress {
    /// Bytes at the last speed measurement.
    last: Option<(Instant, u64)>,
}

impl Progress {
    fn read(&mut self, line: &str, job: &mut StrataJob) {
        if let Some(step) = parse_step(line) {
            job.step = Some(step);
            job.percent = None;
            job.done_bytes = None;
            job.total_bytes = None;
            job.bytes_per_sec = None;
            job.eta_secs = None;
            self.last = None;
            return;
        }
        if let Some((done, total, percent)) = parse_download(line) {
            job.percent = Some(percent);
            job.done_bytes = Some(done);
            job.total_bytes = Some(total);
            let now = Instant::now();
            match self.last {
                Some((at, bytes)) if now.duration_since(at) >= Duration::from_secs(1) => {
                    let secs = now.duration_since(at).as_secs_f64();
                    let speed = (done.saturating_sub(bytes) as f64 / secs) as u64;
                    job.bytes_per_sec = Some(speed);
                    job.eta_secs = (speed > 0).then(|| total.saturating_sub(done) / speed);
                    self.last = Some((now, done));
                }
                Some(_) => {}
                None => self.last = Some((now, done)),
            }
            return;
        }
        if let Some(error) = line.trim().strip_prefix("[X]") {
            job.error = Some(error.trim().to_string());
        }
    }
}

/// `=== Step 4: Downloading the model ===` → `4/7: Downloading the model`.
fn parse_step(line: &str) -> Option<String> {
    let rest = line
        .trim()
        .strip_prefix("=== Step ")?
        .strip_suffix(" ===")?;
    let (n, title) = rest.split_once(": ")?;
    n.parse::<u32>().ok()?;
    Some(format!("{n}/7: {title}"))
}

/// `  model.gguf: 12.3 / 68.0 GB (18%)` → bytes done, bytes in total, percent.
fn parse_download(line: &str) -> Option<(u64, u64, u8)> {
    let (_, rest) = line.trim().rsplit_once(": ")?;
    let (amounts, percent) = rest.trim().strip_suffix("%)")?.split_once(" GB (")?;
    let (done, total) = amounts.split_once(" / ")?;
    let gb = |s: &str| s.trim().parse::<f64>().ok().map(|v| (v * 1e9) as u64);
    Some((gb(done)?, gb(total)?, percent.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_setup_steps_and_downloads() {
        assert_eq!(
            parse_step("=== Step 4: Downloading the model ===").as_deref(),
            Some("4/7: Downloading the model")
        );
        assert_eq!(
            parse_download(
                "  Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf: 12.5 / 50.0 GB (25%)"
            ),
            Some((12_500_000_000, 50_000_000_000, 25))
        );
        assert_eq!(parse_download("nothing: here"), None);
    }
}
