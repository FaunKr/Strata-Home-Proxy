//! How far each learned expert profile has learned: the limits of its
//! current round, the tokens the proxy counted for it, and what it learned
//! in all. The PC service owns the files and which profile learns; the proxy
//! counts and ends learning when the first limit is reached.

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use strata_admin_ui::{
    api::{ExpertCounts, RequestOutcome, RequestRow},
    expert::learned_text,
};
use strata_pc_protocol::{ExpertLearning as Learning, PcStatus, StrataJobRequest};

use crate::{agent::AgentClient, clock, events::EventKind, files::write_private, state::AppState};

/// The counts, kept over a restart of the proxy.
const FILE: &str = "expert-learning.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Kept {
    profiles: Vec<Entry>,
    /// The code benchmark holds learning on the PC: lifted when its run
    /// ends, also after a restart of the proxy.
    #[serde(default)]
    code_bench_hold: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Entry {
    /// The PC service's model id.
    model: String,
    profile: String,
    /// A round of learning runs.
    learning: bool,
    token_limit: Option<u64>,
    minute_limit: Option<u64>,
    /// This round's tokens and seconds.
    tokens: u64,
    secs: u64,
    total_tokens: u64,
    total_secs: u64,
    /// The proxy asked the PC service to end learning: a limit was reached.
    #[serde(default)]
    ending: bool,
    /// When the last round finished, in Unix seconds.
    #[serde(default)]
    finished: Option<u64>,
}

impl Entry {
    fn new(model: &str, profile: &str) -> Self {
        Self {
            model: model.to_string(),
            profile: profile.to_string(),
            ..Self::default()
        }
    }

    fn is(&self, model: &str, profile: &str) -> bool {
        self.model == model && self.profile == profile
    }

    fn limit_reached(&self) -> bool {
        self.token_limit.is_some_and(|l| self.tokens >= l)
            || self.minute_limit.is_some_and(|l| self.secs >= l * 60)
    }

    /// Adds the round to the totals.
    fn finish(&mut self) {
        self.total_tokens += self.tokens;
        self.total_secs += self.secs;
        self.tokens = 0;
        self.secs = 0;
        self.learning = false;
        self.ending = false;
        self.finished = Some(clock::unix_now());
    }
}

/// A round to start for a profile: the limits it learns up to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub tokens: Option<u64>,
    pub minutes: Option<u64>,
}

pub struct ExpertLearning {
    kept: Mutex<Kept>,
    /// What the PC service's last status said learns.
    last: Mutex<Option<Learning>>,
    dir: Option<PathBuf>,
}

impl ExpertLearning {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let kept = dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            kept: Mutex::new(kept),
            last: Mutex::new(None),
            dir,
        }
    }

    fn save(&self, kept: &Kept) {
        let Some(dir) = &self.dir else { return };
        let bytes = serde_json::to_vec_pretty(kept).expect("counts serialize");
        if let Err(e) = write_private(dir, FILE, &bytes) {
            tracing::warn!("{FILE} not saved: {e}");
        }
    }

    fn change<T>(&self, change: impl FnOnce(&mut Kept) -> T) -> T {
        let mut kept = self.kept.lock().unwrap();
        let out = change(&mut kept);
        self.save(&kept);
        out
    }

    /// Every profile the proxy counted for, for the Strata page.
    pub fn counts(&self) -> Vec<ExpertCounts> {
        self.kept
            .lock()
            .unwrap()
            .profiles
            .iter()
            .map(|e| ExpertCounts {
                model: e.model.clone(),
                profile: e.profile.clone(),
                learning: e.learning,
                tokens: e.tokens,
                secs: e.secs,
                token_limit: e.token_limit,
                minute_limit: e.minute_limit,
                total_tokens: e.total_tokens,
                total_secs: e.total_secs,
            })
            .collect()
    }

    /// What profile `name` of `model` learned in all, its round included.
    pub fn learned_tokens(&self, model: &str, name: &str) -> Option<u64> {
        self.kept
            .lock()
            .unwrap()
            .profiles
            .iter()
            .find(|e| e.is(model, name))
            .map(|e| e.total_tokens + e.tokens)
    }

    /// Counts a finished request: Strata answered it while a profile learned
    /// and nothing held learning. Cached prompt tokens Strata did not read.
    pub fn note_request(&self, row: &RequestRow) {
        if row.outcome != RequestOutcome::Done {
            return;
        }
        let Some(learning) = self.last.lock().unwrap().clone().filter(|l| !l.held) else {
            return;
        };
        let read = row
            .prompt_tokens
            .unwrap_or(0)
            .saturating_sub(row.cached_tokens.unwrap_or(0));
        let tokens = read + row.output_tokens.unwrap_or(0);
        if tokens == 0 {
            return;
        }
        self.change(|kept| {
            entry(kept, &learning.model, &learning.profile).tokens += tokens;
        });
    }

    /// Takes the round a job is about to start or end, before the job goes
    /// to the PC service; the counts before it, to put back on a refusal.
    fn plan(&self, request: &StrataJobRequest, limits: Limits) -> Option<Kept> {
        let mut kept = self.kept.lock().unwrap();
        let before = kept.clone();
        match request {
            StrataJobRequest::ExpertProfile {
                model,
                profile,
                learn,
            } => {
                // Any job of a model ends the round of its learning profile.
                for e in kept.profiles.iter_mut().filter(|e| e.model == *model) {
                    if e.learning {
                        e.finish();
                    }
                }
                if *learn && let Some(name) = profile {
                    let e = entry(&mut kept, model, name);
                    e.learning = true;
                    e.token_limit = limits.tokens;
                    e.minute_limit = limits.minutes;
                    e.tokens = 0;
                    e.secs = 0;
                }
            }
            StrataJobRequest::DeleteExpertProfile { model, profile } => {
                kept.profiles.retain(|e| !e.is(model, profile));
            }
            StrataJobRequest::Delete { model } => {
                kept.profiles.retain(|e| e.model != *model);
            }
            _ => return None,
        }
        self.save(&kept);
        Some(before)
    }

    /// Puts the counts from before a refused job back.
    fn undo(&self, before: Kept) {
        self.change(|kept| {
            kept.profiles = before.profiles;
        });
    }

    pub fn code_bench_hold(&self) -> bool {
        self.kept.lock().unwrap().code_bench_hold
    }

    pub fn set_code_bench_hold(&self, on: bool) {
        self.change(|kept| kept.code_bench_hold = on);
    }

    /// The PC service reported a learning profile on its active model, or
    /// one of its models learns by its last report.
    pub fn any_learning(&self, state: &AppState) -> bool {
        self.last.lock().unwrap().is_some()
            || state.strata.last_info().is_some_and(|info| {
                info.models
                    .iter()
                    .any(|m| m.expert_profiles.as_ref().is_some_and(|p| p.learning))
            })
    }
}

fn entry<'a>(kept: &'a mut Kept, model: &str, profile: &str) -> &'a mut Entry {
    let at = match kept.profiles.iter().position(|e| e.is(model, profile)) {
        Some(at) => at,
        None => {
            kept.profiles.push(Entry::new(model, profile));
            kept.profiles.len() - 1
        }
    };
    &mut kept.profiles[at]
}

/// Takes over what the PC service's status says learns: the round's
/// seconds, the end of a round the proxy ended, and a limit reached, which
/// ends learning. Waits while a Strata job or the code benchmark runs.
pub async fn note_status(state: &AppState, agent: &AgentClient, status: &PcStatus) {
    let ours = &state.expert_learning;
    *ours.last.lock().unwrap() = status.expert_learning.clone();
    let now = status.expert_learning.clone();
    let busy = status.strata_job || state.strata.job_running() || state.code_bench.busy();
    // A run that ended without lifting its hold, e.g. over a restart.
    if !busy && ours.code_bench_hold() {
        lift(state).await;
        return;
    }
    let (learned, to_end) = ours.change(|kept| {
        let mut learned = Vec::new();
        // A round the proxy ended is over once the profile no longer learns.
        if !busy {
            for e in kept.profiles.iter_mut().filter(|e| e.ending) {
                if now.as_ref().is_some_and(|l| e.is(&l.model, &l.profile)) {
                    // The job did not end it, e.g. it failed: asked again.
                    e.ending = false;
                    continue;
                }
                learned.push((e.model.clone(), e.profile.clone(), e.tokens, e.secs));
                e.finish();
            }
        }
        let mut to_end = None;
        if let Some(l) = &now {
            let e = entry(kept, &l.model, &l.profile);
            e.learning = true;
            e.secs = e.secs.max(l.secs);
            if !l.held && !busy && !e.ending && e.limit_reached() {
                e.ending = true;
                to_end = Some((e.model.clone(), e.profile.clone()));
            }
        }
        (learned, to_end)
    });
    for (model, profile, tokens, secs) in learned {
        state.events.record(
            EventKind::ExpertProfileLearned,
            Some(format!(
                "Expert profile {profile} of {model} learned: {}",
                learned_text(tokens, secs)
            )),
        );
    }
    if let Some((model, profile)) = to_end {
        let request = StrataJobRequest::ExpertProfile {
            model: model.clone(),
            profile: Some(profile.clone()),
            learn: false,
        };
        match agent.start_strata_job(&request).await {
            Ok(_) => {
                state.strata.set_job_running(true);
                state.overview.refresh_now();
            }
            Err(e) => {
                tracing::warn!("Learning of {profile} not ended: {e}");
                ours.change(|kept| entry(kept, &model, &profile).ending = false);
            }
        }
    }
}

/// Sends a profile job from the web UI with the counts it changes, putting
/// them back when the PC service refuses it.
pub async fn send(
    state: &AppState,
    agent: &AgentClient,
    request: &StrataJobRequest,
    limits: Limits,
) -> Result<strata_pc_protocol::StrataJob, String> {
    let before = state.expert_learning.plan(request, limits);
    let sent = agent.start_strata_job(request).await;
    if sent.is_err()
        && let Some(before) = before
    {
        state.expert_learning.undo(before);
    }
    sent
}

/// Holds learning on the PC for a code benchmark run when a profile
/// learns; waits for the job, so the run's first request finds it held.
pub async fn hold_for_code_bench(
    state: &AppState,
    wait: impl AsyncFnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if !state.expert_learning.any_learning(state) {
        return Ok(());
    }
    let agent = state.agent();
    agent
        .start_strata_job(&StrataJobRequest::HoldLearning { on: true })
        .await?;
    state.expert_learning.set_code_bench_hold(true);
    state.strata.set_job_running(true);
    wait().await
}

/// Lifts the code benchmark's hold, if it set one; waits for the job.
pub async fn lift_code_bench_hold(
    state: &AppState,
    wait: impl AsyncFnOnce() -> Result<(), String>,
) {
    if lift(state).await
        && let Err(e) = wait().await
    {
        tracing::warn!("Lifting the hold on learning failed: {e}");
    }
}

/// Sends the job that lifts the code benchmark's hold, once; true when it
/// went out. A refusal keeps the hold to be lifted at a later status.
async fn lift(state: &AppState) -> bool {
    let ours = &state.expert_learning;
    if !ours.change(|kept| std::mem::replace(&mut kept.code_bench_hold, false)) {
        return false;
    }
    match state
        .agent()
        .start_strata_job(&StrataJobRequest::HoldLearning { on: false })
        .await
    {
        Ok(_) => {
            state.strata.set_job_running(true);
            true
        }
        Err(e) => {
            tracing::warn!("Hold on learning not lifted: {e}");
            ours.set_code_bench_hold(true);
            false
        }
    }
}
