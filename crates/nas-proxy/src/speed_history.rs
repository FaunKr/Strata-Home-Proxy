//! Every speed benchmark run the proxy saw in the PC service's reports,
//! each with what the model ran with. The PC service keeps only each
//! model's last run, and adds the error of a later failed run to it.

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use strata_admin_ui::api::{RunPreset, RunSettings, SpeedModelRuns, SpeedRun};
use strata_pc_protocol::{StrataBenchmark, StrataInfo};

use crate::{clock, files::write_private};

const FILE: &str = "speed-benchmarks.json";

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    next_id: u64,
    runs: Vec<SpeedRun>,
    /// A report was taken before: its records are known, and new ones get
    /// their settings.
    #[serde(skip)]
    seeded: bool,
}

pub struct SpeedHistory {
    kept: Mutex<Kept>,
    /// The file does not read: nothing is taken or saved.
    broken: bool,
    dir: Option<PathBuf>,
}

impl SpeedHistory {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let (kept, broken) = match data_dir.as_ref().map(|dir| std::fs::read(dir.join(FILE))) {
            Some(Ok(bytes)) => match serde_json::from_slice::<Kept>(&bytes) {
                Ok(kept) => (
                    Kept {
                        seeded: true,
                        ..kept
                    },
                    false,
                ),
                // Saving over it would lose every run: it stays as it is
                // until it is repaired, and the proxy keeps no history.
                Err(e) => {
                    tracing::error!("{FILE} not readable, no runs are kept until it is: {e}");
                    (Kept::default(), true)
                }
            },
            _ => (Kept::default(), false),
        };
        Self {
            kept: Mutex::new(kept),
            dir: data_dir.filter(|_| !broken),
            broken,
        }
    }

    /// Takes the runs of `info` the history does not hold yet. The records
    /// of the first report ever taken are older than the history and get no
    /// settings.
    pub fn take(&self, info: &StrataInfo, now: u64) {
        if self.broken {
            return;
        }
        let mut kept = self.kept.lock().unwrap();
        let seeded = kept.seeded;
        let mut changed = !seeded;
        // An older PC service keeps only the last run.
        let records = if info.benchmarks.is_empty() {
            info.benchmark.as_slice()
        } else {
            info.benchmarks.as_slice()
        };
        for record in records {
            // A failed run keeps the model's last measurement next to its
            // error: the measurement is a run of its own.
            let measured = !record.result.is_empty();
            let settings = seeded.then(|| run_settings(info, &record.model, Some(record)));
            // The measurement next to a failure tells what an earlier run
            // was made with, not the failed one.
            let failed_settings =
                seeded.then(|| run_settings(info, &record.model, (!measured).then_some(record)));
            if measured
                && !kept.runs.iter().any(|r| {
                    !r.failed && r.record.model == record.model && r.record.at == record.at
                })
            {
                let id = next_id(&mut kept);
                kept.runs.push(SpeedRun {
                    id,
                    at: record.at,
                    at_text: String::new(),
                    record: StrataBenchmark {
                        error: None,
                        ..record.clone()
                    },
                    failed: false,
                    settings,
                });
                changed = true;
            }
            if record.error.is_some() && !kept.runs.iter().any(|r| same_failure(r, record)) {
                let id = next_id(&mut kept);
                kept.runs.push(SpeedRun {
                    id,
                    // Without a measurement the record is the failed run
                    // itself; with one, it carries the measurement's time.
                    at: if measured { now } else { record.at },
                    at_text: String::new(),
                    record: record.clone(),
                    failed: true,
                    settings: failed_settings,
                });
                changed = true;
            }
        }
        kept.seeded = true;
        if changed {
            self.save(&kept);
        }
    }

    /// Each model's runs, newest first; `info` is the last report, which
    /// says which models are installed.
    pub fn page(&self, info: Option<&StrataInfo>) -> Vec<SpeedModelRuns> {
        let kept = self.kept.lock().unwrap();
        let mut models: Vec<SpeedModelRuns> = Vec::new();
        for run in &kept.runs {
            let run = SpeedRun {
                at_text: clock::local_text(run.at),
                record: StrataBenchmark {
                    at_text: clock::local_text(run.record.at),
                    ..run.record.clone()
                },
                ..run.clone()
            };
            match models.iter_mut().find(|m| m.model == run.record.model) {
                Some(m) => m.runs.push(run),
                None => models.push(SpeedModelRuns {
                    model: run.record.model.clone(),
                    installed: info
                        .is_some_and(|i| i.models.iter().any(|m| m.id == run.record.model)),
                    runs: vec![run],
                }),
            }
        }
        for m in &mut models {
            m.runs.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
        }
        models.sort_by(|a, b| a.model.cmp(&b.model));
        models
    }

    fn save(&self, kept: &Kept) {
        if let Some(dir) = &self.dir {
            let bytes = serde_json::to_vec_pretty(kept).expect("runs serialize");
            if let Err(e) = write_private(dir, FILE, &bytes) {
                tracing::error!("{FILE} not saved: {e}");
            }
        }
    }
}

fn next_id(kept: &mut Kept) -> u64 {
    kept.next_id += 1;
    kept.next_id
}

/// The failed run `record` reports is `run`: a later failure with the same
/// error on the same measurement cannot be told apart from it.
fn same_failure(run: &SpeedRun, record: &StrataBenchmark) -> bool {
    run.failed
        && run.record.model == record.model
        && run.record.at == record.at
        && run.record.error == record.error
}

/// What `model` runs with by `info`; a benchmark's `record` knows what it
/// was measured with better than the model's config now.
pub fn run_settings(
    info: &StrataInfo,
    model: &str,
    record: Option<&StrataBenchmark>,
) -> RunSettings {
    let entry = info.models.iter().find(|m| m.id == model);
    RunSettings {
        context: record
            .and_then(|r| r.context)
            .or_else(|| entry.and_then(|m| m.context)),
        speed_projection: record
            .and_then(|r| r.speed_projection)
            .or_else(|| entry.and_then(|m| m.speed_projection)),
        strata_version: record
            .and_then(|r| r.version.clone())
            .or_else(|| info.version.clone()),
        preset: entry
            .and_then(|m| m.presets.as_ref())
            .and_then(|p| p.latest.as_ref())
            .filter(|p| p.active)
            .map(|p| RunPreset {
                name: p.name.clone(),
                settings: p.settings.clone(),
            }),
        env: entry.and_then(|m| m.env.clone()),
        engine_options: entry.and_then(|m| m.user_args.clone()),
        sampling: entry.and_then(|m| m.sampling.clone()),
    }
}
