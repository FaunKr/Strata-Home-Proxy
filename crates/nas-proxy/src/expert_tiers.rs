//! Expert tiers: where the routed experts of a request's decode ran, from
//! the GPU's expert cache in VRAM, copied over PCIe, or computed by the CPU
//! from RAM. After each finished request that reached Strata the proxy
//! reads Strata's finished requests (`/metrics?requests=all`) and adds each
//! one it has not counted yet: its output tokens split by those shares, per
//! day (NAS time zone), model and tier, in the history's database
//! (docs/adr/0003). The metrics endpoint shows the sums (docs/metrics.md).
//!
//! A Strata request counts once: the ids of the latest read are kept, and
//! only an id not among them is new. The first read ever only keeps its
//! ids, so the counter starts at 0 with the release that brought it.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{clock, lifecycle::PcState, state::AppState, strata_client::StrataRequest};

/// The tables `create_tables` of the history adds.
pub const TABLES: &str = "CREATE TABLE IF NOT EXISTS expert_tokens (
     day TEXT NOT NULL,
     model TEXT NOT NULL,
     tier TEXT NOT NULL,
     tokens REAL NOT NULL,
     PRIMARY KEY (day, model, tier)
 );
 CREATE TABLE IF NOT EXISTS expert_tokens_seen (
     id INTEGER PRIMARY KEY CHECK (id = 0),
     ids TEXT NOT NULL
 );";

/// Where a request's routed experts ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Vram,
    Pcie,
    Cpu,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Vram, Tier::Pcie, Tier::Cpu];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vram => "vram",
            Self::Pcie => "pcie",
            Self::Cpu => "cpu",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label() == label)
    }
}

/// Output tokens per tier.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TierTokens {
    pub vram: f64,
    pub pcie: f64,
    pub cpu: f64,
}

impl TierTokens {
    pub fn get(&self, tier: Tier) -> f64 {
        match tier {
            Tier::Vram => self.vram,
            Tier::Pcie => self.pcie,
            Tier::Cpu => self.cpu,
        }
    }

    fn slot(&mut self, tier: Tier) -> &mut f64 {
        match tier {
            Tier::Vram => &mut self.vram,
            Tier::Pcie => &mut self.pcie,
            Tier::Cpu => &mut self.cpu,
        }
    }

    pub fn add(&mut self, other: &TierTokens) {
        for tier in Tier::ALL {
            *self.slot(tier) += other.get(tier);
        }
    }

    /// One Strata request's output tokens split by its tiers; `None`
    /// without a hit rate, a PCIe share or output tokens. The hit rate's
    /// lookups leave out the experts that went over PCIe.
    pub fn of(request: &StrataRequest) -> Option<Self> {
        let n = request.output_tokens.filter(|n| *n > 0)? as f64;
        let hit = request.hit_rate?.clamp(0.0, 1.0);
        let pcie = request.pcie_share?.clamp(0.0, 1.0);
        Some(Self {
            vram: n * (1.0 - pcie) * hit,
            pcie: n * pcie,
            cpu: n * (1.0 - pcie) * (1.0 - hit),
        })
    }
}

/// What one read adds: the tokens of the requests not in `seen`, `None`
/// when none of them counts. Without `seen` (the first read ever) nothing.
pub fn new_tokens(
    seen: Option<&HashSet<String>>,
    requests: &[StrataRequest],
) -> Option<TierTokens> {
    let seen = seen?;
    requests
        .iter()
        .filter(|r| !seen.contains(&r.id))
        .filter_map(TierTokens::of)
        .reduce(|mut sum, t| {
            sum.add(&t);
            sum
        })
}

/// Adds the requests of a read for `model` on `day` and keeps their ids as
/// the ones seen, in one transaction.
pub fn record(
    db: &mut Connection,
    day: &str,
    model: &str,
    requests: &[StrataRequest],
) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    let seen: Option<HashSet<String>> = tx
        .query_row(
            "SELECT ids FROM expert_tokens_seen WHERE id = 0",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|ids| serde_json::from_str(&ids).unwrap_or_default());
    if let Some(tokens) = new_tokens(seen.as_ref(), requests) {
        for tier in Tier::ALL {
            tx.execute(
                "INSERT INTO expert_tokens (day, model, tier, tokens) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (day, model, tier) DO UPDATE SET tokens = tokens + excluded.tokens",
                params![day, model, tier.label(), tokens.get(tier)],
            )?;
        }
    }
    let ids: Vec<&str> = requests.iter().map(|r| r.id.as_str()).collect();
    tx.execute(
        "INSERT OR REPLACE INTO expert_tokens_seen (id, ids) VALUES (0, ?1)",
        [serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into())],
    )?;
    tx.commit()
}

/// Every model's tokens per tier over all days.
pub fn totals(db: &Connection) -> rusqlite::Result<BTreeMap<String, TierTokens>> {
    let mut statement =
        db.prepare("SELECT model, tier, SUM(tokens) FROM expert_tokens GROUP BY model, tier")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })?;
    let mut out: BTreeMap<String, TierTokens> = BTreeMap::new();
    for row in rows {
        let (model, tier, tokens) = row?;
        if let Some(tier) = Tier::from_label(&tier) {
            *out.entry(model).or_default().slot(tier) += tokens;
        }
    }
    Ok(out)
}

/// The reads of Strata's finished requests: one at a time.
#[derive(Default)]
pub struct ExpertTiers {
    reading: Mutex<Reading>,
}

#[derive(Default)]
struct Reading {
    running: bool,
    /// The model of the latest request that finished during a read: one
    /// more read follows, for all of them, counted for that model.
    next: Option<String>,
}

impl ExpertTiers {
    /// A finished request that reached Strata, counting for `model`: reads
    /// Strata's finished requests in the background, after a read that
    /// still runs.
    pub fn note_request(&self, state: &Arc<AppState>, model: &str) {
        if model.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        {
            let mut reading = self.reading.lock().unwrap();
            if reading.running {
                reading.next = Some(model.to_string());
                return;
            }
            reading.running = true;
        }
        let state = Arc::clone(state);
        let model = model.to_string();
        runtime.spawn(async move {
            let running = Running(Arc::clone(&state));
            state.expert_tiers.reads(&state, model).await;
            drop(running);
        });
    }

    /// Reads for `model`, then once more while requests were noted meanwhile.
    async fn reads(&self, state: &Arc<AppState>, mut model: String) {
        loop {
            read(state, &model).await;
            let mut reading = self.reading.lock().unwrap();
            match reading.next.take() {
                Some(next) => model = next,
                None => {
                    reading.running = false;
                    return;
                }
            }
        }
    }
}

/// Ends the reads when a read panics: a later request starts them again.
struct Running(Arc<AppState>);

impl Drop for Running {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let mut reading = match self.0.expert_tiers.reading.lock() {
            Ok(reading) => reading,
            Err(poisoned) => poisoned.into_inner(),
        };
        reading.running = false;
        reading.next = None;
    }
}

/// One read of Strata's finished requests, counted for `model`; never
/// wakes the PC.
async fn read(state: &Arc<AppState>, model: &str) {
    if matches!(state.pc_state(), PcState::Asleep | PcState::Waking) {
        return;
    }
    let Some(client) = state.strata() else {
        return;
    };
    let requests = match client.finished_requests().await {
        Ok(requests) => requests,
        Err(e) => {
            tracing::debug!("expert tiers not read: {e}");
            return;
        }
    };
    let db = state.events.database();
    let day = clock::day_key(clock::unix_now());
    let model = model.to_string();
    let written = tokio::task::spawn_blocking(move || {
        record(&mut db.lock().unwrap(), &day, &model, &requests).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|written| written);
    if let Err(e) = written {
        tracing::warn!("expert tiers not saved: {e}");
    }
}
