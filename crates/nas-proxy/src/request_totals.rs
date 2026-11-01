//! Request totals: requests and tokens per day, API key, model and outcome,
//! for the metrics endpoint. One row in the history's database
//! (docs/adr/0003) per day (in the NAS's time zone), key name, model and
//! outcome, written with each finished request log row and never trimmed,
//! unlike the request log. A model is named by the PC service's id, as
//! request energy names it; the image model by Strata's name; a request
//! without a model (refused before Strata) by an empty name.

use rusqlite::{Connection, params};
use strata_admin_ui::api::{RequestApi, RequestOutcome, RequestRow};

use crate::{clock, state::AppState};

/// The table `create_tables` of the history adds.
pub const TABLE: &str = "CREATE TABLE IF NOT EXISTS request_totals (
     day TEXT NOT NULL,
     key TEXT NOT NULL,
     model TEXT NOT NULL,
     outcome TEXT NOT NULL,
     requests INTEGER NOT NULL,
     prompt_tokens INTEGER NOT NULL,
     cached_tokens INTEGER NOT NULL,
     output_tokens INTEGER NOT NULL,
     PRIMARY KEY (day, key, model, outcome)
 );";

/// How a request ended, as the metrics endpoint counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    Answered,
    Refused,
    Failed,
}

impl Outcome {
    pub fn of(outcome: &RequestOutcome) -> Self {
        match outcome {
            RequestOutcome::Done | RequestOutcome::CutOff => Self::Answered,
            RequestOutcome::Refused { .. } => Self::Refused,
            RequestOutcome::StrataError { .. }
            | RequestOutcome::NoAnswer
            | RequestOutcome::Lost
            | RequestOutcome::CancelledByClient => Self::Failed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        [Self::Answered, Self::Refused, Self::Failed]
            .into_iter()
            .find(|o| o.label() == label)
    }
}

/// The model a finished request counts for: the image model by Strata's
/// name, any other by the PC service's id (`request_energy::model_for`),
/// none without a model.
pub fn model_for(state: &AppState, row: &RequestRow) -> String {
    match (&row.api, row.model.as_deref()) {
        (_, None) => String::new(),
        (RequestApi::Images, Some(name)) => name.to_string(),
        (_, Some(name)) => crate::request_energy::model_for(state, name),
    }
}

/// Adds the finished `row` to its day, key, `model` and outcome.
pub(crate) fn add(db: &Connection, row: &RequestRow, model: &str) -> rusqlite::Result<()> {
    let tokens = |t: Option<u64>| t.unwrap_or(0) as i64;
    db.execute(
        "INSERT INTO request_totals \
         (day, key, model, outcome, requests, prompt_tokens, cached_tokens, output_tokens) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7) \
         ON CONFLICT (day, key, model, outcome) DO UPDATE SET \
         requests = requests + 1, \
         prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
         cached_tokens = cached_tokens + excluded.cached_tokens, \
         output_tokens = output_tokens + excluded.output_tokens",
        params![
            clock::day_key(row.at),
            row.key,
            model,
            Outcome::of(&row.outcome).label(),
            tokens(row.prompt_tokens),
            tokens(row.cached_tokens),
            tokens(row.output_tokens),
        ],
    )
    .map(|_| ())
}

/// The sums over all days of one key, model and outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Total {
    pub key: String,
    pub model: String,
    pub outcome: Outcome,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
}

/// Every key, model and outcome with its sums over all days.
pub fn totals(db: &Connection) -> rusqlite::Result<Vec<Total>> {
    let mut statement = db.prepare(
        "SELECT key, model, outcome, SUM(requests), SUM(prompt_tokens), \
         SUM(cached_tokens), SUM(output_tokens) \
         FROM request_totals GROUP BY key, model, outcome",
    )?;
    let rows = statement.query_map([], |row| {
        let outcome: String = row.get(2)?;
        let count = |i: usize| row.get::<_, i64>(i).map(|n| n.max(0) as u64);
        Ok(
            Outcome::from_label(&outcome).map(|outcome| -> rusqlite::Result<Total> {
                Ok(Total {
                    key: row.get(0)?,
                    model: row.get(1)?,
                    outcome,
                    requests: count(3)?,
                    prompt_tokens: count(4)?,
                    cached_tokens: count(5)?,
                    output_tokens: count(6)?,
                })
            }),
        )
    })?;
    let mut out = Vec::new();
    for row in rows {
        if let Some(total) = row? {
            out.push(total?);
        }
    }
    Ok(out)
}
