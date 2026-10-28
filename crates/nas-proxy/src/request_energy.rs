//! Request energy: the power plug read every `REQUEST_PLUG_SAMPLE_MS` while
//! requests run, each reading split evenly among the requests running then,
//! and each model's energy per 1000 tokens from one row per day and model in
//! the history's database (docs/adr/0003). A model is named by the PC
//! service's id, as the Strata page lists it, not by Strata's name. The daily rows are never trimmed,
//! unlike the request log.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use rusqlite::{Connection, params};
use strata_admin_ui::api::{EnergyAverage, ModelEnergy, RequestOutcome, RequestRow};
use strata_pc_protocol::StrataInfo;

use crate::{clock, state::AppState};

/// The table `create_tables` of the history adds: one row per day (in the
/// NAS's time zone, `2026-10-28`) and model.
const TABLE: &str = "CREATE TABLE IF NOT EXISTS request_energy (
     day TEXT NOT NULL,
     model TEXT NOT NULL,
     wh REAL NOT NULL,
     output_tokens INTEGER NOT NULL,
     requests INTEGER NOT NULL,
     PRIMARY KEY (day, model)
 );";

/// One row: whether the rows saved before request energy counted for the
/// PC service's model ids still wait for their move.
const MOVE_TABLE: &str = "CREATE TABLE IF NOT EXISTS request_energy_move (
     pending INTEGER NOT NULL
 );";

/// Creates the tables for `create_tables` of the history. A table of
/// daily rows from an earlier version, which saved them under the names
/// Strata answered with, gets its move; a new one never does.
pub(crate) fn create_tables(db: &Connection) -> rusqlite::Result<()> {
    let first = !exists(db, "request_energy_move")?;
    let earlier = exists(db, "request_energy")?;
    db.execute_batch(TABLE)?;
    db.execute_batch(MOVE_TABLE)?;
    if first {
        db.execute(
            "INSERT INTO request_energy_move (pending) VALUES (?1)",
            [earlier],
        )?;
    }
    Ok(())
}

fn exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// A request needs this many readings for its energy.
const MIN_READINGS: u32 = 2;

/// The days the shorter average covers, today included.
const RECENT_DAYS: i64 = 30;

/// One running request's shares of the readings so far.
#[derive(Clone, Copy, Debug, Default)]
struct Shares {
    watts: f64,
    readings: u32,
}

pub struct RequestEnergy {
    db: Arc<Mutex<Connection>>,
    /// The requests sent to Strata and not finished, by their request log id.
    running: Mutex<BTreeMap<u64, Shares>>,
    /// The loop that reads the plug runs.
    sampling: AtomicBool,
    /// The rows of an earlier version wait for their move.
    move_pending: AtomicBool,
}

impl RequestEnergy {
    /// The figures in `db`, which holds the table `request_energy`.
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        let pending = db
            .lock()
            .unwrap()
            .query_row("SELECT pending FROM request_energy_move", [], |row| {
                row.get::<_, bool>(0)
            })
            .unwrap_or(false);
        Self {
            db,
            running: Mutex::default(),
            sampling: AtomicBool::new(false),
            move_pending: AtomicBool::new(pending),
        }
    }

    /// Moves the rows of an earlier version to the model `info` marks as
    /// active, once; read on a blocking thread. Without a model marked
    /// active the rows wait for the next report.
    pub async fn move_saved(&self, info: &StrataInfo) {
        if !self.move_pending.load(Ordering::SeqCst) {
            return;
        }
        let db = Arc::clone(&self.db);
        let info = info.clone();
        let moved = tokio::task::spawn_blocking(move || move_rows(&mut db.lock().unwrap(), &info))
            .await
            .unwrap_or(Ok(false));
        self.note_move(moved);
    }

    /// `move_saved` at the proxy's start, with the report kept from before.
    pub fn move_saved_at_start(&self, info: &StrataInfo) {
        if self.move_pending.load(Ordering::SeqCst) {
            self.note_move(move_rows(&mut self.db.lock().unwrap(), info));
        }
    }

    fn note_move(&self, moved: rusqlite::Result<bool>) {
        match moved {
            Ok(true) => self.move_pending.store(false, Ordering::SeqCst),
            Ok(false) => {}
            Err(e) => tracing::warn!("request energy not moved: {e}"),
        }
    }

    /// Adds one request's energy to the day of `at` (Unix seconds) for
    /// `model`.
    pub fn record(&self, at: u64, model: &str, wh: f64, output_tokens: u64) {
        let db = self.db.lock().unwrap();
        if let Err(e) = add(&db, at, model, wh, output_tokens) {
            tracing::warn!("request energy not kept: {e}");
        }
    }

    /// Each model's two averages at today's `price_ct_per_kwh`, models with
    /// the most recent days first; `info` is the PC service's last report,
    /// which says which models are installed. Read on a blocking thread.
    pub async fn averages(
        &self,
        price_ct_per_kwh: f64,
        info: Option<&StrataInfo>,
    ) -> Vec<ModelEnergy> {
        let db = Arc::clone(&self.db);
        let since = clock::day_key_days_ago(clock::unix_now(), RECENT_DAYS - 1);
        let sums = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            sums(&db, &since).unwrap_or_else(|e| {
                tracing::warn!("request energy not read: {e}");
                Vec::new()
            })
        })
        .await
        .unwrap_or_default();
        sums.into_iter()
            .map(|(model, recent, all)| ModelEnergy {
                installed: info.is_some_and(|i| i.models.iter().any(|m| m.id == model)),
                model,
                last_30_days: recent.average(price_ct_per_kwh),
                all_time: all.average(price_ct_per_kwh),
            })
            .collect()
    }

    /// The request `id` went to Strata: it takes its share of each reading
    /// from now on, and the plug is read while any request runs.
    /// Without a power plug nothing runs: no request gets energy then.
    pub fn start(&self, state: &Arc<AppState>, id: u64) {
        if state.cfg().shelly_url.is_none() {
            return;
        }
        self.running.lock().unwrap().insert(id, Shares::default());
        if !self.sampling.swap(true, Ordering::SeqCst) {
            tokio::spawn(sample(Arc::clone(state)));
        }
    }

    /// Ends the request `id`, sent to Strata at `sent`, as `row` finished:
    /// its energy in Wh, or `None` for a request without an answer or
    /// without output tokens, or with fewer than `MIN_READINGS` readings.
    pub fn end(&self, id: u64, sent: Instant, row: &RequestRow) -> Option<f64> {
        let shares = self.running.lock().unwrap().remove(&id)?;
        let answered =
            row.outcome == RequestOutcome::Done && row.output_tokens.is_some_and(|t| t > 0);
        (answered && shares.readings >= MIN_READINGS).then(|| {
            let mean = shares.watts / f64::from(shares.readings);
            mean * sent.elapsed().as_secs_f64() / 3600.0
        })
    }

    /// Takes a reading of `watts` for the requests running now.
    fn share(&self, watts: f64) {
        let mut running = self.running.lock().unwrap();
        let each = watts / running.len().max(1) as f64;
        for shares in running.values_mut() {
            shares.watts += each;
            shares.readings += 1;
        }
    }

    /// Ends the loop when no request runs; `false` if one started meanwhile
    /// and the loop goes on.
    fn stop_if_idle(&self) -> bool {
        if !self.running.lock().unwrap().is_empty() {
            return false;
        }
        self.sampling.store(false, Ordering::SeqCst);
        // A request that started between the check and the store found the
        // loop still running and started none.
        self.running.lock().unwrap().is_empty() || self.sampling.swap(true, Ordering::SeqCst)
    }
}

/// Reads the plug every `REQUEST_PLUG_SAMPLE_MS` until no request runs.
/// Without a power plug nothing is read, so no request gets energy.
async fn sample(state: Arc<AppState>) {
    let energy = &state.request_energy;
    loop {
        if energy.stop_if_idle() {
            return;
        }
        let started = Instant::now();
        let cfg = state.cfg();
        if let Some(url) = &cfg.shelly_url
            && let Some(watts) = crate::plug::power(&state.probe_client(), url).await
        {
            energy.share(watts);
        }
        tokio::time::sleep(cfg.request_plug_sample.saturating_sub(started.elapsed())).await;
    }
}

/// The model a request Strata answered as `answered` counts for: the one
/// the PC service's last report marks as active, which is how the Strata
/// page names it. Without one, Strata's name, an alias counting for its
/// model.
pub fn model_for(state: &AppState, answered: &str) -> String {
    state
        .strata
        .last_info()
        .and_then(|info| active(&info).map(str::to_string))
        .unwrap_or_else(|| state.models.model_of(answered))
}

fn active(info: &StrataInfo) -> Option<&str> {
    info.models.iter().find(|m| m.active).map(|m| m.id.as_str())
}

/// Moves every day row not under one of `info`'s models to the one it
/// marks as active, adding up rows of the same day; `false` without a model
/// marked active or without a move waiting.
fn move_rows(db: &mut Connection, info: &StrataInfo) -> rusqlite::Result<bool> {
    let Some(active) = active(info) else {
        return Ok(false);
    };
    let transaction = db.transaction()?;
    let pending: bool =
        transaction.query_row("SELECT pending FROM request_energy_move", [], |row| {
            row.get(0)
        })?;
    if !pending {
        return Ok(false);
    }
    let installed: Vec<&str> = info.models.iter().map(|m| m.id.as_str()).collect();
    let rows: Vec<(String, String, f64, i64, i64)> = transaction
        .prepare("SELECT day, model, wh, output_tokens, requests FROM request_energy")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (day, model, wh, output_tokens, requests) in rows {
        if installed.contains(&model.as_str()) {
            continue;
        }
        transaction.execute(
            "DELETE FROM request_energy WHERE day = ?1 AND model = ?2",
            params![day, model],
        )?;
        transaction.execute(
            "INSERT INTO request_energy (day, model, wh, output_tokens, requests) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (day, model) DO UPDATE SET \
             wh = wh + excluded.wh, \
             output_tokens = output_tokens + excluded.output_tokens, \
             requests = requests + excluded.requests",
            params![day, active, wh, output_tokens, requests],
        )?;
    }
    transaction.execute("UPDATE request_energy_move SET pending = 0", [])?;
    transaction.commit()?;
    Ok(true)
}

/// `wh` in € at `price_ct_per_kwh`.
pub fn eur(wh: f64, price_ct_per_kwh: f64) -> f64 {
    wh / 1000.0 * price_ct_per_kwh / 100.0
}

/// Adds one request to its day's row, in `db`'s lock the caller holds.
pub(crate) fn add(
    db: &Connection,
    at: u64,
    model: &str,
    wh: f64,
    output_tokens: u64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO request_energy (day, model, wh, output_tokens, requests) \
         VALUES (?1, ?2, ?3, ?4, 1) \
         ON CONFLICT (day, model) DO UPDATE SET \
         wh = wh + excluded.wh, \
         output_tokens = output_tokens + excluded.output_tokens, \
         requests = requests + 1",
        params![clock::day_key(at), model, wh, output_tokens as i64],
    )
    .map(|_| ())
}

/// Energy, output tokens and requests over some days.
#[derive(Clone, Copy, Debug, Default)]
struct Sums {
    wh: f64,
    output_tokens: u64,
    requests: u64,
}

impl Sums {
    /// Σ energy ÷ Σ output tokens, so each request weighs by its tokens.
    fn average(self, price_ct_per_kwh: f64) -> Option<EnergyAverage> {
        (self.requests > 0 && self.output_tokens > 0).then(|| {
            let wh_per_1000 = self.wh / self.output_tokens as f64 * 1000.0;
            EnergyAverage {
                wh_per_1000,
                eur_per_1000: eur(wh_per_1000, price_ct_per_kwh),
                requests: self.requests,
            }
        })
    }
}

/// Each model's sums since the day `since` and over all time, the model
/// with the latest day first.
fn sums(db: &Connection, since: &str) -> rusqlite::Result<Vec<(String, Sums, Sums)>> {
    let mut statement = db.prepare(
        "SELECT model, \
         SUM(CASE WHEN day >= ?1 THEN wh ELSE 0 END), \
         SUM(CASE WHEN day >= ?1 THEN output_tokens ELSE 0 END), \
         SUM(CASE WHEN day >= ?1 THEN requests ELSE 0 END), \
         SUM(wh), SUM(output_tokens), SUM(requests) \
         FROM request_energy GROUP BY model ORDER BY MAX(day) DESC, model",
    )?;
    let rows = statement.query_map([since], |row| {
        let sums = |wh: usize| -> rusqlite::Result<Sums> {
            Ok(Sums {
                wh: row.get(wh)?,
                output_tokens: row.get::<_, i64>(wh + 1)?.max(0) as u64,
                requests: row.get::<_, i64>(wh + 2)?.max(0) as u64,
            })
        };
        Ok((row.get(0)?, sums(1)?, sums(4)?))
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_average_is_energy_over_tokens() {
        let sums = Sums {
            wh: 3.0,
            output_tokens: 2000,
            requests: 2,
        };
        let average = sums.average(30.0).unwrap();
        assert!((average.wh_per_1000 - 1.5).abs() < 1e-9);
        assert!((average.eur_per_1000 - 0.00045).abs() < 1e-12);
        assert_eq!(average.requests, 2);
    }

    #[test]
    fn no_requests_have_no_average() {
        assert_eq!(Sums::default().average(30.0), None);
    }
}
