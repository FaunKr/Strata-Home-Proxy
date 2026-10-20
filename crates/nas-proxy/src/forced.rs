//! Forced sleep and forced Strata restart from the web UI. The proxy asks
//! the PC service only while it does not consider the PC asleep or waking,
//! so neither ever wakes the PC.

use std::sync::Arc;

use strata_admin_ui::labels::{NO_AGENT, NO_ANSWER, TOO_OLD_TO_FORCE, strata_job_label};
use strata_pc_protocol::{ForcedRestart, ForcedSleep, StrataJobKind};

use crate::{
    agent::Forced,
    events::EventKind,
    state::{AppState, PcState},
};

/// Asks for a forced sleep; an accepted one counts like an accepted sleep
/// request (Nachlauf, then asleep).
pub async fn sleep(state: &Arc<AppState>, by: &str) -> Result<(), String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    match state.snapshot().pc {
        PcState::Asleep => return Err("The PC is asleep already.".into()),
        PcState::Waking => return Err(WAKING.into()),
        _ => {}
    }
    let answer = agent.force_sleep().await;
    let result = match answer {
        Forced::Done(forced) => {
            state.forced_sleep_accepted(sleep_detail(by, &forced));
            Ok(())
        }
        other => Err(failure(other)),
    };
    state.overview.refresh_now();
    result
}

/// Asks for a forced Strata restart; the answer says whether Strata
/// starts again.
pub async fn restart_strata(state: &Arc<AppState>, by: &str) -> Result<ForcedRestart, String> {
    let agent = state.agent();
    if !agent.configured() {
        return Err(NO_AGENT.into());
    }
    match state.snapshot().pc {
        PcState::Asleep => return Err("The PC is asleep; restarting Strata never wakes it.".into()),
        PcState::Waking => return Err(WAKING.into()),
        _ => {}
    }
    let answer = agent.force_restart().await;
    let result = match answer {
        Forced::Done(forced) => {
            state.events.record(
                EventKind::ForcedStrataRestart,
                Some(restart_detail(by, &forced)),
            );
            Ok(forced)
        }
        other => Err(failure(other)),
    };
    state.overview.refresh_now();
    result
}

const WAKING: &str = "The PC is waking up; try again once it is awake.";

fn failure<T>(answer: Forced<T>) -> String {
    match answer {
        Forced::Done(_) => unreachable!("only failures"),
        Forced::Refused(reason) => reason,
        Forced::TooOld => TOO_OLD_TO_FORCE.into(),
        Forced::NoAnswer => NO_ANSWER.into(),
    }
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
        "Strata stays stopped: nothing to start"
    };
    format!("{by}{}, {started}", cancelled(forced.cancelled_job))
}

fn cancelled(job: Option<StrataJobKind>) -> String {
    job.map(|kind| format!(", cancelled job: {}", strata_job_label(kind)))
        .unwrap_or_default()
}
