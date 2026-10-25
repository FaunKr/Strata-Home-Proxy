//! Keeps the Python package `jsonschema` in the Python that runs Strata.
//!
//! Strata checks an answer to a `response_format` `json_schema` request
//! against the schema only with it; without it only that the answer is one
//! JSON object (`serve/structured.py`). Its setup does not add it, and each
//! update of the Strata Terminal brings a fresh Python without it. So the
//! service installs it with Strata's own command at its start and after an
//! Install or Update job. A failed install changes nothing else: Strata
//! keeps its weaker check until the next try.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    agent::Agent,
    command::CommandSpec,
    job::{JobContext, POLL},
    strata::strata_python,
};

/// Names the check for the test fakes, as the catalog script does.
const CHECK: &str = "# jsonschema\nimport jsonschema\n";
/// The range Strata's own message names (`serve/structured.py`).
const REQUIREMENT: &str = "jsonschema>=4.23,<5";

/// How long the check and the install may take before they are ended.
pub struct Limits {
    inner: Mutex<(Duration, Duration)>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            inner: Mutex::new((Duration::from_secs(60), Duration::from_secs(300))),
        }
    }
}

impl Limits {
    /// For tests: limits they can wait out.
    pub fn set(&self, check: Duration, install: Duration) {
        *self.inner.lock().unwrap() = (check, install);
    }

    fn get(&self) -> (Duration, Duration) {
        *self.inner.lock().unwrap()
    }
}

pub const INSTALLED: &str =
    "jsonschema installed: Strata checks JSON answers against the requested schema";
/// At the service's start Strata may run already, and it notes a missing
/// package once until it restarts.
pub const INSTALLED_FOR_NEXT_START: &str = "jsonschema installed: Strata checks JSON answers against the requested schema from its next start";

/// In an Install or Update job, before Strata starts: installs the package
/// when it is missing and says so in one line of the job's log.
pub(crate) async fn ensure_in_job(job: &JobContext) {
    if job.cancelled() {
        return;
    }
    match ensure(job.agent(), &|| job.cancelled()).await {
        Some(Ok(())) => job.line(INSTALLED),
        Some(Err(why)) => job.line(&not_installed(&why)),
        None => {}
    }
}

/// At the service's start, in the background: as [`ensure_in_job`], into
/// the service's log, but never beside a job: an Install or Update may be
/// replacing Strata's Python, and checks itself when it ends. A job that
/// starts meanwhile ends the check.
pub async fn at_start(agent: Arc<Agent>) {
    let job_runs = || agent.jobs.running();
    if job_runs() {
        return;
    }
    match ensure(&agent, &job_runs).await {
        Some(Ok(())) => tracing::info!("{INSTALLED_FOR_NEXT_START}"),
        Some(Err(why)) => tracing::warn!("{}", not_installed(&why)),
        None => {}
    }
}

fn not_installed(why: &str) -> String {
    format!(
        "jsonschema not installed ({why}): Strata checks JSON answers only for one JSON object until the next try"
    )
}

/// Installs the package when Strata's Python lacks it: `None` when there
/// is nothing to do (it is there, there is no Strata, or `cancelled` held),
/// else whether the install worked. Ends what runs once `cancelled` holds.
async fn ensure(
    agent: &Agent,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Result<(), String>> {
    let python = strata_python(&agent.cfg)?;
    let (check_limit, install_limit) = agent.jsonschema_limits.get();
    let spec = |args: &[&str]| CommandSpec {
        program: python.clone(),
        args: args.iter().map(|a| a.to_string()).collect(),
        dir: python.parent().map(Into::into),
        env: vec![("PYTHONNOUSERSITE".into(), "1".into())],
    };
    let check = exit_within(agent, &spec(&["-c", CHECK]), check_limit, cancelled).await;
    if check.as_ref().is_ok_and(|(code, _)| *code == 0) || cancelled() {
        return None;
    }
    let install = spec(&[
        "-m",
        "pip",
        "install",
        "--only-binary=:all:",
        "--disable-pip-version-check",
        "--no-input",
        "--no-warn-script-location",
        REQUIREMENT,
    ]);
    match exit_within(agent, &install, install_limit, cancelled).await {
        Ok((0, _)) => Some(Ok(())),
        Ok((code, last)) => Some(Err(match last {
            Some(last) => format!("pip ended with code {code}: {last}"),
            None => format!("pip ended with code {code}"),
        })),
        Err(why) => Some(Err(why)),
    }
}

/// The exit code of `spec` and its last line of output; it is ended after
/// `limit` or once `cancelled` holds.
async fn exit_within(
    agent: &Agent,
    spec: &CommandSpec,
    limit: Duration,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(i32, Option<String>), String> {
    let mut run = agent
        .platform()
        .run(spec)
        .map_err(|e| format!("{} did not start: {e}", spec.program.display()))?;
    let cancel = run.canceller();
    let ended = tokio::time::Instant::now() + limit;
    let ran = async move {
        let mut last = None;
        while let Some(line) = run.next_line().await {
            if !line.trim().is_empty() {
                last = Some(line.trim().to_string());
            }
        }
        (run.wait().await, last)
    };
    tokio::pin!(ran);
    // Ticks on its own, however fast the command prints.
    let mut tick = tokio::time::interval(POLL);
    loop {
        tokio::select! {
            (code, last) = &mut ran => {
                return code.map(|code| (code, last)).ok_or_else(|| "it was ended".into());
            }
            _ = tick.tick() => {
                if cancelled() {
                    cancel();
                    return Err("cancelled".into());
                }
                if tokio::time::Instant::now() >= ended {
                    cancel();
                    return Err(format!("no end within {} s", limit.as_secs()));
                }
            }
        }
    }
}
