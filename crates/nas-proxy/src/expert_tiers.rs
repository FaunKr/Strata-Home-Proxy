//! Expert tiers: where the routed experts of a request's decode ran, from
//! the GPU's expert cache in VRAM, copied over PCIe, or computed by the CPU
//! from RAM. After each finished request that reached Strata the proxy
//! reads Strata's request history (`/metrics?requests=all`) and adds each
//! entry it has not counted yet: its output tokens split by those shares,
//! per day (NAS time zone), model and tier, in the history's database
//! (docs/adr/0003). The metrics endpoint shows the sums (docs/metrics.md).
//!
//! An entry counts once: the ids of the latest read are kept, and only an
//! id not among them is new. The first read ever only keeps its ids, so
//! the counter starts at 0 with the release that brought it.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{clock, lifecycle::PcState, state::AppState, strata_client::HistoryEntry};

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

/// Output tokens per tier, in `Tier::ALL`'s order.
pub type Tokens = [f64; 3];

/// One entry's output tokens split by its tiers; `None` without a hit rate,
/// a PCIe share or output tokens. The hit rate's lookups leave out the
/// experts that went over PCIe.
pub fn split(entry: &HistoryEntry) -> Option<Tokens> {
    let n = entry.output_tokens.filter(|n| *n > 0)? as f64;
    let hit = entry.hit_rate?.clamp(0.0, 1.0);
    let pcie = entry.pcie_share?.clamp(0.0, 1.0);
    Some([
        n * (1.0 - pcie) * hit,
        n * pcie,
        n * (1.0 - pcie) * (1.0 - hit),
    ])
}

/// What one read adds: the tokens of the entries not in `seen`, `None` when
/// none of them counts. Without `seen` (the first read ever) nothing.
pub fn new_tokens(seen: Option<&HashSet<String>>, entries: &[HistoryEntry]) -> Option<Tokens> {
    let seen = seen?;
    entries
        .iter()
        .filter(|e| !seen.contains(&e.id))
        .filter_map(split)
        .reduce(|a, b| [a[0] + b[0], a[1] + b[1], a[2] + b[2]])
}

/// Adds the entries of a read for `model` on `day` and keeps their ids as
/// the ones seen, in one transaction.
pub fn record(
    db: &mut Connection,
    day: &str,
    model: &str,
    entries: &[HistoryEntry],
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
    if let Some(tokens) = new_tokens(seen.as_ref(), entries) {
        for (tier, n) in Tier::ALL.into_iter().zip(tokens) {
            tx.execute(
                "INSERT INTO expert_tokens (day, model, tier, tokens) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (day, model, tier) DO UPDATE SET tokens = tokens + excluded.tokens",
                params![day, model, tier.label(), n],
            )?;
        }
    }
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    tx.execute(
        "INSERT OR REPLACE INTO expert_tokens_seen (id, ids) VALUES (0, ?1)",
        [serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into())],
    )?;
    tx.commit()
}

/// Every model's tokens per tier over all days.
pub fn totals(db: &Connection) -> rusqlite::Result<BTreeMap<String, Tokens>> {
    let mut statement =
        db.prepare("SELECT model, tier, SUM(tokens) FROM expert_tokens GROUP BY model, tier")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })?;
    let mut out: BTreeMap<String, Tokens> = BTreeMap::new();
    for row in rows {
        let (model, tier, tokens) = row?;
        if let Some(i) =
            Tier::from_label(&tier).and_then(|t| Tier::ALL.iter().position(|a| *a == t))
        {
            out.entry(model).or_default()[i] += tokens;
        }
    }
    Ok(out)
}

/// The reads of Strata's request history: one at a time.
#[derive(Default)]
pub struct ExpertTiers {
    reading: Mutex<Reading>,
}

#[derive(Default)]
struct Reading {
    running: bool,
    /// The model of a request that finished during a read: one more read
    /// follows, for all of them.
    next: Option<String>,
}

impl ExpertTiers {
    /// A finished request that reached Strata, counting for `model`: reads
    /// Strata's history in the background, after a read that still runs.
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
        runtime.spawn(reads(Arc::clone(state), model.to_string()));
    }
}

/// Reads for `model`, then once more for each model noted meanwhile.
async fn reads(state: Arc<AppState>, mut model: String) {
    loop {
        read(&state, &model).await;
        let mut reading = state.expert_tiers.reading.lock().unwrap();
        match reading.next.take() {
            Some(next) => model = next,
            None => {
                reading.running = false;
                return;
            }
        }
    }
}

/// One read of Strata's history, counted for `model`; never wakes the PC.
async fn read(state: &Arc<AppState>, model: &str) {
    if matches!(state.pc_state(), PcState::Asleep | PcState::Waking) {
        return;
    }
    let Some(client) = state.strata() else {
        return;
    };
    let entries = match client.request_history().await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!("expert tiers not read: {e}");
            return;
        }
    };
    let db = state.events.database();
    let day = clock::day_key(clock::unix_now());
    let model = model.to_string();
    let written = tokio::task::spawn_blocking(move || {
        record(&mut db.lock().unwrap(), &day, &model, &entries)
    })
    .await;
    match written {
        Ok(Err(e)) => tracing::warn!("expert tiers not saved: {e}"),
        Err(e) => tracing::warn!("expert tiers not saved: {e}"),
        Ok(Ok(())) => {}
    }
}
