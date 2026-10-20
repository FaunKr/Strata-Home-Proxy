//! Time limits that depend on how fast the model is (#190): how long Strata
//! may take for the update summary, and how long opencode may work on one
//! code benchmark task. A value set on the settings page or in the
//! environment applies as is; without one, the limit is suggested from the
//! slowest installed model in the last benchmarks the PC service reported.
//! The Proxy keeps those in memory only, so after a restart the assumed
//! speeds apply until the PC service reports again.

use std::time::Duration;

use strata_pc_protocol::{StrataBenchmark, StrataInfo};

use crate::state::AppState;

/// Speeds and context assumed where a benchmark does not say.
const ASSUMED_DECODE_TPS: f64 = 30.0;
const ASSUMED_PROMPT_TPS: f64 = 300.0;
const ASSUMED_CONTEXT: u32 = 8192;
/// Tokens the summary's answer takes: medium reasoning plus six bullets.
const SUMMARY_ANSWER_TOKENS: f64 = 2000.0;
/// Tokens opencode writes on one task, about.
const TASK_TOKENS: f64 = 50_000.0;
/// Room on top of the estimate.
const MARGIN: f64 = 2.0;

/// A suggested limit and what it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub secs: u64,
    /// E.g. "slowest model IQ3_XXS at 12 tok/s".
    pub from: String,
}

/// The suggestion for a setting of this module, `None` for every other key.
pub fn suggestion(state: &AppState, key: &str) -> Option<Suggestion> {
    let info = state.strata.last_info();
    let speeds = Speeds::slowest(info.as_ref());
    match key {
        "UPDATE_SUMMARY_TIMEOUT_SECS" => Some(speeds.update_summary()),
        "CODE_BENCH_TASK_TIMEOUT_SECS" => Some(speeds.code_bench_task()),
        _ => None,
    }
}

/// The limit for the update summary's Strata request.
pub fn update_summary(state: &AppState) -> Duration {
    state.cfg().update_summary_timeout.unwrap_or_else(|| {
        let info = state.strata.last_info();
        Duration::from_secs(Speeds::slowest(info.as_ref()).update_summary().secs)
    })
}

/// The limit for each task of a code benchmark run.
pub fn code_bench_task(state: &AppState) -> Duration {
    state.cfg().code_bench_task_timeout.unwrap_or_else(|| {
        let info = state.strata.last_info();
        Duration::from_secs(Speeds::slowest(info.as_ref()).code_bench_task().secs)
    })
}

/// The speeds a suggestion is worked out from.
#[derive(Debug, PartialEq)]
struct Speeds {
    /// The model they were measured on; `None` when all are assumed.
    model: Option<String>,
    decode_tps: f64,
    prompt_tps: f64,
    context: u32,
}

impl Speeds {
    /// The installed model with the lowest decode speed; figures its
    /// benchmark lacks are assumed.
    fn slowest(info: Option<&StrataInfo>) -> Self {
        let installed =
            |b: &&StrataBenchmark| info.is_some_and(|i| i.models.iter().any(|m| m.id == b.model));
        let slowest = info
            .into_iter()
            .flat_map(|i| &i.benchmarks)
            .filter(installed)
            .filter(|b| b.decode_tps.is_some_and(|t| t > 0.0))
            .min_by(|a, b| a.decode_tps.unwrap().total_cmp(&b.decode_tps.unwrap()));
        match slowest {
            Some(b) => Self {
                model: Some(b.model.clone()),
                decode_tps: b.decode_tps.unwrap(),
                prompt_tps: b
                    .prompt_tps
                    .filter(|t| *t > 0.0)
                    .unwrap_or(ASSUMED_PROMPT_TPS),
                context: b.context.filter(|c| *c > 0).unwrap_or(ASSUMED_CONTEXT),
            },
            None => Self {
                model: None,
                decode_tps: ASSUMED_DECODE_TPS,
                prompt_tps: ASSUMED_PROMPT_TPS,
                context: ASSUMED_CONTEXT,
            },
        }
    }

    /// Reading half the context, as the summary's prompt is trimmed to,
    /// and writing the answer.
    fn update_summary(&self) -> Suggestion {
        let read = f64::from(self.context) / 2.0 / self.prompt_tps;
        let write = SUMMARY_ANSWER_TOKENS / self.decode_tps;
        self.suggest((read + write) * MARGIN)
    }

    fn code_bench_task(&self) -> Suggestion {
        self.suggest(TASK_TOKENS / self.decode_tps * MARGIN)
    }

    fn suggest(&self, secs: f64) -> Suggestion {
        let from = match &self.model {
            Some(model) => format!("slowest model {model} at {} tok/s", tps(self.decode_tps)),
            None => format!(
                "assumed {} tok/s, run Compare models for a better value",
                tps(ASSUMED_DECODE_TPS)
            ),
        };
        Suggestion {
            secs: full_minutes(secs),
            from,
        }
    }
}

/// Rounded up to the full minute, at least one.
fn full_minutes(secs: f64) -> u64 {
    let minutes = (secs / 60.0).ceil().max(1.0);
    minutes as u64 * 60
}

/// A speed with at most one decimal, e.g. 12 or 38.5.
fn tps(t: f64) -> String {
    ((t * 10.0).round() / 10.0).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speeds(decode_tps: f64, prompt_tps: f64, context: u32) -> Speeds {
        Speeds {
            model: Some("m".into()),
            decode_tps,
            prompt_tps,
            context,
        }
    }

    #[test]
    fn assumed_speeds_give_three_and_56_minutes() {
        let assumed = Speeds::slowest(None);
        assert_eq!(assumed.update_summary().secs, 180);
        assert_eq!(assumed.code_bench_task().secs, 56 * 60);
    }

    #[test]
    fn suggestions_round_up_to_the_minute_with_one_at_least() {
        // About 1 s.
        assert_eq!(speeds(100_000.0, 8192.0, 8192).update_summary().secs, 60);
        assert_eq!(full_minutes(60.0), 60);
        assert_eq!(full_minutes(60.5), 120);
        assert_eq!(full_minutes(0.0), 60);
    }

    #[test]
    fn a_slow_prompt_speed_lengthens_the_summary_only() {
        // 65536 / 2 / 50 = 655.4 s reading, 2000 / 40 = 50 s writing.
        let s = speeds(40.0, 50.0, 65536);
        assert_eq!(s.update_summary().secs, 24 * 60);
        assert_eq!(s.code_bench_task().secs, 42 * 60);
    }

    #[test]
    fn speeds_show_at_most_one_decimal() {
        assert_eq!(tps(12.0), "12");
        assert_eq!(tps(38.54), "38.5");
    }
}
