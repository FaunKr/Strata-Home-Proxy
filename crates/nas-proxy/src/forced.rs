//! Forced sleep and forced Strata restart from the web UI. The proxy asks
//! the PC service only while it does not consider the PC asleep or waking,
//! so neither ever wakes the PC.

use std::{future::Future, sync::Arc};

use strata_admin_ui::labels::{NO_AGENT, NO_ANSWER, TOO_OLD_TO_FORCE, strata_job_label};
use strata_pc_protocol::{ForcedRestart, ForcedSleep, StrataJobKind};

use crate::{
    agent::{AgentClient, Forced},
    events::EventKind,
    state::{AppState, PcState},
};

/// Asks for a forced sleep; an accepted one counts like an accepted sleep
/// request (Nachlauf, then asleep).
pub async fn sleep(state: &Arc<AppState>, by: &str) -> Result<(), String> {
    ask(
        state,
        "The PC is asleep already.",
        |agent| async move { agent.force_sleep().await },
        |forced| state.forced_sleep_accepted(sleep_detail(by, forced)),
    )
    .await
    .map(|_| ())
}

/// Asks for a forced Strata restart; the answer says whether Strata
/// starts again.
pub async fn restart_strata(state: &Arc<AppState>, by: &str) -> Result<ForcedRestart, String> {
    ask(
        state,
        "The PC is asleep; restarting Strata never wakes it.",
        |agent| async move { agent.force_restart().await },
        |forced| {
            state.events.record(
                EventKind::ForcedStrataRestart,
                Some(restart_detail(by, forced)),
            );
        },
    )
    .await
}

/// Sends a forced action with `send` to a configured PC service, unless
/// the PC counts as asleep (`asleep` says so) or waking. `done` notes an
/// accepted one; the overview refreshes after any answer.
async fn ask<T, F>(
    state: &Arc<AppState>,
    asleep: &str,
    send: impl FnOnce(AgentClient) -> F,
    done: impl FnOnce(&T),
) -> Result<T, String>
where
    F: Future<Output = Forced<T>>,
{
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    match state.snapshot().pc {
        PcState::Asleep => return Err(asleep.into()),
        PcState::Waking => {
            return Err("The PC is waking up; try again once it is awake.".into());
        }
        _ => {}
    }
    let result = match send(agent).await {
        Forced::Done(forced) => {
            done(&forced);
            Ok(forced)
        }
        Forced::Refused(reason) => Err(reason),
        Forced::TooOld => Err(TOO_OLD_TO_FORCE.into()),
        Forced::NoAnswer => Err(NO_ANSWER.into()),
    };
    state.overview.refresh_now();
    result
}

/// `from the web UI, overriding user activity, gaming mode, cancelled job: …`
fn sleep_detail(by: &str, forced: &ForcedSleep) -> String {
    let overridden: Vec<&str> = [
        (forced.user_active, "user activity"),
        (forced.strata_busy, "a request in Strata"),
        (forced.gaming_mode, "gaming mode"),
    ]
    .into_iter()
    .filter_map(|(on, what)| on.then_some(what))
    .collect();
    let mut detail = by.to_string();
    if !overridden.is_empty() {
        detail.push_str(&format!(", overriding {}", overridden.join(", ")));
    }
    detail.push_str(&cancelled(forced.cancelled_job));
    detail
}

/// `from the web UI, cancelled job: …, Strata started again`
fn restart_detail(by: &str, forced: &ForcedRestart) -> String {
    let started = if forced.started {
        "Strata started again"
    } else {
        "Strata not started again"
    };
    format!("{by}{}, {started}", cancelled(forced.cancelled_job))
}

fn cancelled(job: Option<StrataJobKind>) -> String {
    job.map(|kind| format!(", cancelled job: {}", strata_job_label(kind)))
        .unwrap_or_default()
}
