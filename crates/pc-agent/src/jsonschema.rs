//! Keeps the Python package `jsonschema` in the Python that runs Strata.
//!
//! Strata checks an answer to a `response_format` `json_schema` request
//! against the schema only with it; without it only that the answer is one
//! JSON object (`serve/structured.py`). Its setup does not add it, and each
//! update of the Strata Terminal brings a fresh Python without it. So the
//! service installs it with Strata's own command at its start and after an
//! Install or Update job. A failed install changes nothing else: Strata
//! keeps its weaker check until the next try.

use std::{sync::Arc, time::Duration};

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
const CHECK_LIMIT: Duration = Duration::from_secs(60);
const INSTALL_LIMIT: Duration = Duration::from_secs(300);

pub const INSTALLED: &str =
    "jsonschema installed: Strata checks JSON answers against the requested schema";

/// In an Install or Update job, before Strata starts: installs the package
/// when it is missing and says so in one line of the job's log.
pub(crate) async fn ensure_in_job(job: &JobContext) {
    if job.cancelled() {
        return;
    }
    if let Some(line) = ensure(job.agent(), &|| job.cancelled()).await {
        job.line(&line);
    }
}

/// At the service's start, in the background: as [`ensure_in_job`], but
/// not while a job runs (an update may be replacing Strata's Python; the
/// job checks itself), and into the service's log.
pub async fn at_start(agent: Arc<Agent>) {
    if agent.jobs.running() {
        return;
    }
    if let Some(line) = ensure(&agent, &|| false).await {
        tracing::info!("{line}");
    }
}

/// Installs the package when Strata's Python lacks it; what happened, if
/// anything did. Ends what runs once `cancelled` holds.
async fn ensure(agent: &Agent, cancelled: &(dyn Fn() -> bool + Sync)) -> Option<String> {
    let python = strata_python(&agent.cfg)?;
    let spec = |args: &[&str]| CommandSpec {
        program: python.clone(),
        args: args.iter().map(|a| a.to_string()).collect(),
        dir: python.parent().map(Into::into),
        env: vec![("PYTHONNOUSERSITE".into(), "1".into())],
    };
    if exit_within(agent, &spec(&["-c", CHECK]), CHECK_LIMIT, cancelled).await == Ok(0)
        || cancelled()
    {
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
    let why = match exit_within(agent, &install, INSTALL_LIMIT, cancelled).await {
        Ok(0) => return Some(INSTALLED.into()),
        Ok(code) => format!("pip ended with code {code}"),
        Err(why) => why,
    };
    Some(format!(
        "jsonschema not installed ({why}): Strata checks JSON answers only for one JSON object until the next try"
    ))
}

/// The exit code of `spec`, which is ended after `limit` or once
/// `cancelled` holds; its output is not kept.
async fn exit_within(
    agent: &Agent,
    spec: &CommandSpec,
    limit: Duration,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<i32, String> {
    let mut run = agent
        .platform()
        .run(spec)
        .map_err(|e| format!("{} did not start: {e}", spec.program.display()))?;
    let cancel = run.canceller();
    let ended = tokio::time::Instant::now() + limit;
    loop {
        tokio::select! {
            line = run.next_line() => if line.is_none() { break },
            () = tokio::time::sleep(POLL) => {
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
    run.wait().await.ok_or_else(|| "it was ended".into())
}
