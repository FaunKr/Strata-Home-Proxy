//! Request energy: the power plug read every `REQUEST_PLUG_SAMPLE_MS` while
//! requests run, each reading split evenly among the requests running then,
//! and each model's energy per 1000 tokens from one row per day and model in
//! the history's database (docs/adr/0003). The daily rows are never trimmed,
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
use strata_admin_ui::api::{EnergyAverage, ModelEnergy};
use strata_pc_protocol::StrataInfo;

use crate::{clock, state::AppState};

/// The table `create_tables` of the history adds: one row per day (in the
/// NAS's time zone, `2026-10-28`) and model.
pub const TABLE: &str = "CREATE TABLE IF NOT EXISTS request_energy (
     day TEXT NOT NULL,
     model TEXT NOT NULL,
     wh REAL NOT NULL,
     output_tokens INTEGER NOT NULL,
     requests INTEGER NOT NULL,
     PRIMARY KEY (day, model)
 );";

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
}

impl RequestEnergy {
    /// The figures in `db`, which holds the table `request_energy`.
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self {
            db,
            running: Mutex::default(),
            sampling: AtomicBool::new(false),
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
    pub fn start(&self, state: &Arc<AppState>, id: u64) {
        self.running.lock().unwrap().insert(id, Shares::default());
        if !self.sampling.swap(true, Ordering::SeqCst) {
            tokio::spawn(sample(Arc::clone(state)));
        }
    }

    /// Ends the request `id`, sent to Strata at `sent`: its energy in Wh, or
    /// `None` with fewer than `MIN_READINGS` readings.
    pub fn end(&self, id: u64, sent: Instant) -> Option<f64> {
        let shares = self.running.lock().unwrap().remove(&id)?;
        (shares.readings >= MIN_READINGS).then(|| {
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

/// Adds one request to its day's row, in `db`'s lock the caller holds.
pub fn add(
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
                eur_per_1000: wh_per_1000 / 1000.0 * price_ct_per_kwh / 100.0,
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
