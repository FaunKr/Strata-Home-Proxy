//! The running Strata job and the context each job works through.
//!
//! A job script (see [`crate::strata`]) never touches the job's state
//! itself: it shows its steps, learns of a cancel, waits, runs commands and
//! cleans up through its [`JobContext`]. Only this module and the HTTP
//! handlers (status, current job, cancel) see the state, through [`Jobs`].

use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use strata_pc_protocol::{StrataJob, StrataJobKind, StrataJobState, TuneSpeed};

use crate::{
    agent::Agent,
    command::{Cancel, CommandSpec},
    terminal::{UpdateEvent, update_event},
};

const JOB_FILE: &str = "strata-job.json";
/// How often a job looks again while it waits.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// Lines of output a job keeps, for the page to show after a failure.
pub(crate) const KEPT_LINES: usize = 20;

/// What a command's output lines are read for, besides being kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    Lines,
    /// Setup's steps, downloads and errors.
    Setup,
    /// The Terminal's update events.
    Update,
}

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

    /// The kind of the running job.
    pub fn running_kind(&self) -> Option<StrataJobKind> {
        self.inner.lock().unwrap().running().map(|j| j.kind)
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
                speeds: Vec::new(),
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
            let step = prefixed(inner.step_prefix.as_deref(), step.to_string());
            if let Some(job) = inner.current.as_mut() {
                job.step = Some(step);
                clear_figures(job);
            }
        });
        self.save();
    }

    /// Shows a step with its own progress, as a tool reports it.
    pub fn progress(&self, step: &str, percent: Option<u8>, eta_secs: Option<u64>) {
        let changed = self.with(|inner| {
            let job = inner.current.as_mut()?;
            let changed = job.step.as_deref() != Some(step) || job.percent != percent;
            job.step = Some(step.to_string());
            job.percent = percent;
            job.eta_secs = eta_secs;
            Some(changed)
        });
        if changed == Some(true) {
            self.save();
        }
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

    /// A running tune's state from Strata's status file: its step, share
    /// and time left, a new line for the log when the step is new, and the
    /// measurements so far.
    pub fn tune_status(
        &self,
        line: Option<String>,
        step: &str,
        percent: Option<u8>,
        eta_secs: Option<u64>,
        speeds: Vec<TuneSpeed>,
    ) {
        let changed = self.with(|inner| {
            let job = inner.current.as_mut()?;
            let changed = line.is_some()
                || job.step.as_deref() != Some(step)
                || job.percent != percent
                || job.speeds != speeds;
            if let Some(line) = line {
                keep_line(&mut job.lines, line);
            }
            job.step = Some(step.to_string());
            job.percent = percent;
            job.eta_secs = eta_secs;
            job.speeds = speeds;
            Some(changed)
        });
        if changed == Some(true) {
            self.save();
        }
    }

    /// Adds `line` to the job's output.
    pub(crate) fn line(&self, line: &str) {
        self.with(|inner| {
            if let Some(job) = inner.current.as_mut() {
                keep_line(&mut job.lines, line.to_string());
            }
        });
        self.save();
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
        let reading = if progress {
            Reading::Setup
        } else {
            Reading::Lines
        };
        self.run_command(spec, reading, None, true).await
    }

    /// Like [`command`](Self::command) without setup's progress, and gives
    /// every line of the output too, also after a failure.
    pub(crate) async fn command_output(
        &self,
        spec: CommandSpec,
    ) -> (Result<(), Step>, Vec<String>) {
        let mut output = Vec::new();
        let ran = self
            .run_command(spec, Reading::Lines, Some(&mut output), true)
            .await;
        (ran, output)
    }

    /// Like [`command_output`](Self::command_output), but a line holding a
    /// JSON object, the command's answer, stays out of the job's lines.
    pub(crate) async fn command_answer(
        &self,
        spec: CommandSpec,
    ) -> (Result<(), Step>, Vec<String>) {
        let mut output = Vec::new();
        let ran = self
            .run_command(spec, Reading::Lines, Some(&mut output), false)
            .await;
        (ran, output)
    }

    /// Like [`command_output`](Self::command_output) for a Terminal command
    /// with `--output jsonl`: its events become the job's steps, log lines
    /// and download progress, and no JSON line is shown.
    pub(crate) async fn command_events(
        &self,
        spec: CommandSpec,
    ) -> (Result<(), Step>, Vec<String>) {
        let mut output = Vec::new();
        let ran = self
            .run_command(spec, Reading::Update, Some(&mut output), false)
            .await;
        (ran, output)
    }

    async fn run_command(
        &self,
        spec: CommandSpec,
        reading: Reading,
        mut output: Option<&mut Vec<String>>,
        show_answers: bool,
    ) -> Result<(), Step> {
        let mut run = self
            .agent
            .platform()
            .run(&spec)
            .map_err(|e| Step::Failed(e.to_string()))?;
        let cancelled = self.with(|inner| {
            inner.bench = BenchReading::default();
            inner.progress = Progress::default();
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
            if let Some(output) = output.as_mut() {
                output.push(line.clone());
            }
            if reading == Reading::Update
                && let Some(event) = update_event(&line)
            {
                let changed = self.with(|inner| {
                    let JobsInner {
                        current,
                        progress,
                        secret,
                        step_prefix,
                        ..
                    } = inner;
                    let job = current.as_mut()?;
                    let before = (job.step.clone(), job.percent);
                    progress.read_update(event, job, secret.as_deref(), step_prefix.as_deref());
                    Some((job.step.clone(), job.percent) != before)
                });
                // Strata reports a download up to four times a second: the
                // job is saved for a new step or percent only.
                if changed == Some(true) {
                    self.save();
                }
                continue;
            }
            if !show_answers
                && serde_json::from_str::<serde_json::Value>(line.trim())
                    .is_ok_and(|v| v.is_object())
            {
                continue;
            }
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
                if reading == Reading::Setup {
                    p.read(&line, job);
                    // In a comparison: which model the step belongs to.
                    // A step shown again after a file check has it already.
                    if job.step != before
                        && let (Some(prefix), Some(step)) =
                            (step_prefix.as_deref(), job.step.as_mut())
                        && !step.starts_with(&format!("{prefix} · "))
                    {
                        *step = prefixed(Some(prefix), std::mem::take(step));
                    }
                }
                keep_line(&mut job.lines, line);
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

/// Reads setup's output: the step, a download's progress, a file check's
/// progress, an error. The same lines Strata's own MCP tool reads
/// (`tools/strata_mcp.py`). Also the Terminal's update events.
#[derive(Default)]
struct Progress {
    /// The number of setup's last step heading.
    step: Option<u32>,
    /// What the last figure counted, e.g. `model.gguf` or
    /// `checking model.gguf`; a new one starts the speed over.
    label: Option<String>,
    /// While setup checks a file: the step to show again after it.
    resume: Option<Option<String>>,
    /// Bytes at the last speed measurement.
    last: Option<(Instant, u64)>,
    /// The last line was setup's error: the next one may be its hint.
    after_error: bool,
}

impl Progress {
    fn read(&mut self, line: &str, job: &mut StrataJob) {
        self.read_at(line, job, Instant::now());
    }

    /// One event of `strata update --output jsonl`: a phase or a wait is the
    /// step and a log line, a download's figures come from Strata itself.
    fn read_update(
        &mut self,
        event: UpdateEvent,
        job: &mut StrataJob,
        secret: Option<&str>,
        prefix: Option<&str>,
    ) {
        let step = match event {
            UpdateEvent::Phase(message) | UpdateEvent::Waiting(message) => message,
            UpdateEvent::Download {
                done,
                total,
                percent,
                bytes_per_sec,
            } => {
                job.percent = Some(percent);
                job.done_bytes = Some(done);
                job.total_bytes = Some(total);
                job.bytes_per_sec = bytes_per_sec;
                job.eta_secs = bytes_per_sec
                    .filter(|speed| *speed > 0)
                    .map(|speed| total.saturating_sub(done) / speed);
                return;
            }
        };
        *self = Self::default();
        let step = match secret {
            Some(key) => step.replace(key, "***"),
            None => step,
        };
        // A wait is reported again at each look; one line says it.
        if job.lines.last() != Some(&step) {
            keep_line(&mut job.lines, step.clone());
        }
        job.step = Some(prefixed(prefix, step));
        clear_figures(job);
    }

    fn read_at(&mut self, line: &str, job: &mut StrataJob, now: Instant) {
        let hint = std::mem::take(&mut self.after_error);
        if let Some((n, title)) = parse_step(line) {
            job.step = Some(step_text(n, title));
            job.percent = None;
            job.done_bytes = None;
            job.total_bytes = None;
            job.bytes_per_sec = None;
            job.eta_secs = None;
            *self = Self {
                step: Some(n),
                ..Self::default()
            };
            return;
        }
        if let Some((label, done, total, percent)) = parse_download(line) {
            if self.label.as_deref() != Some(label) {
                self.label = Some(label.to_string());
                self.last = None;
                job.bytes_per_sec = None;
                job.eta_secs = None;
                // Setup hashes a big file after its download and prints the
                // same figure for it (Strata 1972af6).
                match (label.strip_prefix("checking "), self.resume.is_some()) {
                    (Some(file), _) => {
                        let checking = format!("Checking {file}");
                        let step = match self.step {
                            Some(n) => step_text(n, &checking),
                            None => checking,
                        };
                        let before = job.step.replace(step);
                        self.resume.get_or_insert(before);
                    }
                    (None, true) => job.step = self.resume.take().flatten(),
                    (None, false) => {}
                }
            }
            job.percent = Some(percent);
            job.done_bytes = Some(done);
            job.total_bytes = Some(total);
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
        // Setup's `fail` prints the error and on the next line what to do
        // about it, indented; then a blank line and "Setup stopped …".
        if let Some(error) = line.trim().strip_prefix("[X]") {
            job.error = Some(error.trim().to_string());
            self.after_error = true;
        } else if hint
            && line.starts_with("       ")
            && let Some(error) = job.error.as_mut()
        {
            let stop = if error.ends_with(['.', '!', '?', ':']) {
                ""
            } else {
                "."
            };
            *error = format!("{error}{stop} {}", line.trim());
        }
    }
}

/// `=== Step 4: Downloading the model ===` → 4 and `Downloading the model`.
fn parse_step(line: &str) -> Option<(u32, &str)> {
    let rest = line
        .trim()
        .strip_prefix("=== Step ")?
        .strip_suffix(" ===")?;
    let (n, title) = rest.split_once(": ")?;
    Some((n.parse().ok()?, title))
}

/// How a job shows setup's step `n`: `4/7: Downloading the model`.
fn step_text(n: u32, title: &str) -> String {
    format!("{n}/7: {title}")
}

/// `  model.gguf: 12.3 / 68.0 GB (18%)` → the label (`model.gguf`), bytes
/// done, bytes in total, percent.
fn parse_download(line: &str) -> Option<(&str, u64, u64, u8)> {
    let (label, rest) = line.trim().rsplit_once(": ")?;
    let (amounts, percent) = rest.trim().strip_suffix("%)")?.split_once(" GB (")?;
    let (done, total) = amounts.split_once(" / ")?;
    let gb = |s: &str| s.trim().parse::<f64>().ok().map(|v| (v * 1e9) as u64);
    Some((label, gb(done)?, gb(total)?, percent.trim().parse().ok()?))
}

/// Adds `line` to a job's output, which keeps the last [`KEPT_LINES`].
/// `step`, after which model of a comparison it belongs to, if any.
fn prefixed(prefix: Option<&str>, step: String) -> String {
    match prefix {
        Some(prefix) => format!("{prefix} · {step}"),
        None => step,
    }
}

/// A new step starts without the last one's share, download and estimate.
fn clear_figures(job: &mut StrataJob) {
    job.percent = None;
    job.done_bytes = None;
    job.total_bytes = None;
    job.bytes_per_sec = None;
    job.eta_secs = None;
}

fn keep_line(lines: &mut Vec<String>, line: String) {
    lines.push(line);
    let over = lines.len().saturating_sub(KEPT_LINES);
    lines.drain(..over);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_setup_steps_and_downloads() {
        assert_eq!(
            parse_step("=== Step 4: Downloading the model ==="),
            Some((4, "Downloading the model"))
        );
        assert_eq!(
            parse_download(
                "  Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf: 12.5 / 50.0 GB (25%)"
            ),
            Some((
                "Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf",
                12_500_000_000,
                50_000_000_000,
                25
            ))
        );
        assert_eq!(parse_download("nothing: here"), None);
    }

    fn job() -> StrataJob {
        StrataJob {
            id: None,
            kind: StrataJobKind::Install,
            model: None,
            state: StrataJobState::Running,
            step: None,
            percent: None,
            done_bytes: None,
            total_bytes: None,
            bytes_per_sec: None,
            eta_secs: None,
            lines: Vec::new(),
            speeds: Vec::new(),
            error: None,
            result: None,
            started: 0,
            ended: None,
        }
    }

    /// Feeds `lines` one second apart, as setup prints them.
    fn feed(progress: &mut Progress, job: &mut StrataJob, start: Instant, lines: &[&str]) {
        for (i, line) in lines.iter().enumerate() {
            progress.read_at(line, job, start + Duration::from_secs(i as u64));
        }
    }

    /// What setup's `fail` prints when the feed has no DeepSeek engine for
    /// the card (`setup.py`, Strata 7dd5cf18), with the engine folder as on
    /// Windows.
    const NO_DEEPSEEK_ENGINE: &[&str] = &[
        "=== Step 4: the DeepSeek engine ===",
        "",
        "  [X]  DeepSeek's engine is not available for this computer yet",
        "       Supported graphics cards: NVIDIA RTX 30 and RTX 50 series, and the Radeon AI PRO R9700. If you downloaded the DeepSeek engine package from the release page (the file strata-dsv41-...-windows-....zip), run the same command again with --dsv41-package PATH-OF-THE-FILE (strata launch accepts it too), or put the file in C:\\Strata\\engine-dsv41 and run it again. Strata checks it before it is used. Or update Strata later (a newer version may add your card), or pick another model. Nothing has been downloaded.",
        "",
        "Setup stopped. Fix the item above and run it again - everything already done is kept and skipped.",
    ];

    #[test]
    fn setup_s_error_comes_with_its_hint() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(&mut progress, &mut job, start, NO_DEEPSEEK_ENGINE);
        let error = job.error.unwrap();
        assert!(
            error.starts_with(
                "DeepSeek's engine is not available for this computer yet. Supported graphics cards: NVIDIA RTX 30"
            ),
            "{error}"
        );
        assert!(error.ends_with("Nothing has been downloaded."), "{error}");
    }

    #[test]
    fn an_error_without_a_hint_stays_as_it_is() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "  [X] out of memory",
                "",
                "Setup stopped. Fix the item above and run it again.",
            ],
        );
        assert_eq!(job.error.as_deref(), Some("out of memory"));
    }

    #[test]
    fn a_file_check_is_shown_as_checking_and_the_next_download_as_the_step() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "=== Step 5: downloading Qwen 3.8 Flash ===",
                "  a-00001-of-00002.gguf:  50.00 / 50.00 GB (100%)",
                "  checking a-00001-of-00002.gguf (SHA-256, 50.0 GB) ...",
                "  checking a-00001-of-00002.gguf:  12.50 / 50.00 GB (25%)",
            ],
        );
        assert_eq!(
            job.step.as_deref(),
            Some("5/7: Checking a-00001-of-00002.gguf")
        );
        assert_eq!(job.percent, Some(25));
        assert_eq!(job.done_bytes, Some(12_500_000_000));
        assert_eq!(job.total_bytes, Some(50_000_000_000));

        progress.read_at(
            "  a-00002-of-00002.gguf:   1.00 / 20.00 GB (5%)",
            &mut job,
            start + Duration::from_secs(10),
        );
        assert_eq!(job.step.as_deref(), Some("5/7: downloading Qwen 3.8 Flash"));
        assert_eq!(job.percent, Some(5));
    }

    #[test]
    fn a_check_counts_in_any_step_and_a_new_heading_replaces_it() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "=== Step 4: the Strata engine ===",
                "  checking vulkan-sdk.tar:   0.60 / 1.20 GB (50%)",
            ],
        );
        assert_eq!(job.step.as_deref(), Some("4/7: Checking vulkan-sdk.tar"));
        progress.read_at(
            "=== Step 5: downloading Qwen 3.8 Flash ===",
            &mut job,
            start + Duration::from_secs(5),
        );
        assert_eq!(job.step.as_deref(), Some("5/7: downloading Qwen 3.8 Flash"));
        assert_eq!(job.percent, None);
    }

    #[test]
    fn without_a_heading_the_step_from_before_the_check_comes_back() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        job.step = Some("Updating Strata".into());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "  checking vulkan-sdk.tar:   0.60 / 1.20 GB (50%)",
                "  checking vulkan-sdk.tar:   1.20 / 1.20 GB (100%)",
            ],
        );
        assert_eq!(job.step.as_deref(), Some("Checking vulkan-sdk.tar"));
        progress.read_at(
            "  engine.zip:   0.10 / 0.50 GB (20%)",
            &mut job,
            start + Duration::from_secs(5),
        );
        assert_eq!(job.step.as_deref(), Some("Updating Strata"));
    }

    #[test]
    fn checks_of_two_files_in_a_row_both_show_and_the_heading_comes_back() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "=== Step 5: downloading Qwen 3.8 Flash ===",
                "  checking a-00001.gguf:  50.00 / 50.00 GB (100%)",
                "  checking a-00002.gguf:   0.50 / 20.00 GB (2%)",
            ],
        );
        assert_eq!(job.step.as_deref(), Some("5/7: Checking a-00002.gguf"));
        progress.read_at(
            "  mmproj.gguf:   0.10 / 1.00 GB (10%)",
            &mut job,
            start + Duration::from_secs(5),
        );
        assert_eq!(job.step.as_deref(), Some("5/7: downloading Qwen 3.8 Flash"));
    }

    #[test]
    fn the_speed_starts_over_with_each_file() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "  checking a.gguf:  48.00 / 50.00 GB (96%)",
                "  checking a.gguf:  50.00 / 50.00 GB (100%)",
                "  checking b.gguf:   0.50 / 20.00 GB (2%)",
            ],
        );
        // Not measured against a.gguf's bytes.
        assert_eq!(job.bytes_per_sec, None);
        progress.read_at(
            "  checking b.gguf:   1.50 / 20.00 GB (7%)",
            &mut job,
            start + Duration::from_secs(3),
        );
        assert_eq!(job.bytes_per_sec, Some(1_000_000_000));
    }

    #[test]
    fn the_speed_starts_over_when_downloading_and_checking_switch() {
        let (mut progress, mut job, start) = (Progress::default(), job(), Instant::now());
        feed(
            &mut progress,
            &mut job,
            start,
            &[
                "=== Step 5: downloading Qwen 3.8 Flash ===",
                "  a.gguf:  48.00 / 50.00 GB (96%)",
                "  a.gguf:  50.00 / 50.00 GB (100%)",
            ],
        );
        assert_eq!(job.bytes_per_sec, Some(2_000_000_000));
        // The check's first figure has nothing of its own to measure against.
        progress.read_at(
            "  checking a.gguf:   1.00 / 50.00 GB (2%)",
            &mut job,
            start + Duration::from_secs(3),
        );
        assert_eq!(job.bytes_per_sec, None);
        assert_eq!(job.eta_secs, None);
        progress.read_at(
            "  checking a.gguf:   1.50 / 50.00 GB (3%)",
            &mut job,
            start + Duration::from_secs(4),
        );
        assert_eq!(job.bytes_per_sec, Some(500_000_000));
        progress.read_at(
            "  b.gguf:   0.10 / 20.00 GB (0%)",
            &mut job,
            start + Duration::from_secs(5),
        );
        assert_eq!(job.bytes_per_sec, None);
    }
}
