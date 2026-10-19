//! The Verlauf: Ereignisse about waking and sleeping and the Leistung of the
//! Messsteckdose over time, for the web UI and the power estimate. Both live in
//! a SQLite database in `DATA_DIR` (docs/adr/0003); without one the store is
//! in memory.

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub use strata_admin_ui::api::EventKind;

/// How many of the most recent events `recent` and `latest_pc` see; the
/// database keeps all of them.
const KEEP_IN_MEMORY: usize = 1000;

/// Set once `events.jsonl` is imported, so an import that failed is tried
/// again on the next start, even when new events were recorded meanwhile.
const IMPORTED: i64 = 1;

/// How often the proxy notes that it still runs. After a stop the Verlauf is
/// unknown from the last note on, so it is off by at most this long.
pub const BEAT: Duration = Duration::from_secs(60);

const SELECT: &str = "SELECT ts, kind, detail FROM events";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Unix time in seconds.
    pub ts: u64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One reading of the Messsteckdose. Kept forever, like the Ereignisse.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix time in seconds.
    pub ts: u64,
    pub watts: f64,
}

pub struct History {
    /// The latest events, for the overview, without asking the database.
    recent: Mutex<VecDeque<Event>>,
    db: Arc<Mutex<Connection>>,
}

impl History {
    /// Opens the database and imports `events.jsonl` before returning; meant
    /// for startup.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let dir = data_dir.and_then(|dir| match fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                tracing::warn!("DATA_DIR {} nicht nutzbar: {e}", dir.display());
                None
            }
        });
        let db = open_database(dir.as_deref());
        // The history so far, so the overview shows it after a restart too.
        let mut recent = select(
            &db,
            &format!("{SELECT} ORDER BY ts DESC, rowid DESC LIMIT ?1"),
            &[&(KEEP_IN_MEMORY as i64)],
        );
        recent.reverse();
        Self {
            recent: Mutex::new(recent.into()),
            db: Arc::new(Mutex::new(db)),
        }
    }

    /// Written at once, like the appended line before, so a killed proxy
    /// loses nothing. A single insert in WAL mode takes no sync to disk.
    pub fn record(&self, kind: EventKind, detail: Option<String>) {
        let event = Event {
            ts: now(),
            kind,
            detail,
        };
        tracing::info!(?kind, detail = event.detail.as_deref(), "Ereignis");
        if let Err(e) = insert(&self.db.lock().unwrap(), &event) {
            tracing::warn!("Verlauf nicht geschrieben: {e}");
        }
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == KEEP_IN_MEMORY {
            recent.pop_front();
        }
        recent.push_back(event);
    }

    /// Notes that the proxy still runs; the next start records the last note
    /// as `ProxyStopped`.
    pub fn beat(&self) {
        let written = self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO alive (id, ts) VALUES (0, ?1)",
            [to_sql(now())],
        );
        if let Err(e) = written {
            tracing::warn!("Lebenszeichen nicht geschrieben: {e}");
        }
    }

    /// The most recent events, oldest first.
    pub fn recent(&self) -> Vec<Event> {
        self.recent.lock().unwrap().iter().cloned().collect()
    }

    /// The latest `n` events about the PC, newest first.
    pub fn latest_pc(&self, n: usize) -> Vec<Event> {
        self.recent
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|e| !e.kind.is_admin() && !e.kind.is_history_mark())
            .take(n)
            .cloned()
            .collect()
    }

    /// Every event with `from <= ts <= to`, oldest first, back to the first
    /// one. The query runs on a blocking thread, not on the async runtime.
    pub async fn range(&self, from: u64, to: u64) -> Vec<Event> {
        let [from, to] = bounds(from, to);
        self.blocking(move |db| {
            select(
                db,
                &format!("{SELECT} WHERE ts BETWEEN ?1 AND ?2 ORDER BY ts, rowid"),
                &[&from, &to],
            )
        })
        .await
    }

    /// Written at once, like `record`, so a killed proxy loses nothing.
    pub fn record_sample(&self, sample: Sample) {
        let written = self.db.lock().unwrap().execute(
            "INSERT INTO samples (ts, watts) VALUES (?1, ?2)",
            params![to_sql(sample.ts), sample.watts],
        );
        if let Err(e) = written {
            tracing::warn!("Messwert der Steckdose nicht geschrieben: {e}");
        }
    }

    /// Every sample with `from <= ts <= to`, oldest first. The query runs on a
    /// blocking thread, not on the async runtime.
    pub async fn samples(&self, from: u64, to: u64) -> Vec<Sample> {
        let [from, to] = bounds(from, to);
        self.blocking(move |db| {
            let rows = db
                .prepare(
                    "SELECT ts, watts FROM samples WHERE ts BETWEEN ?1 AND ?2 ORDER BY ts, rowid",
                )
                .and_then(|mut statement| {
                    statement
                        .query_map([from, to], |row| {
                            Ok(Sample {
                                ts: row.get::<_, i64>(0)? as u64,
                                watts: row.get(1)?,
                            })
                        })
                        .and_then(Iterator::collect)
                });
            rows.unwrap_or_else(|e| {
                tracing::warn!("Messwerte der Steckdose nicht gelesen: {e}");
                Vec::new()
            })
        })
        .await
    }

    /// Runs `query` on a blocking thread, not on the async runtime.
    async fn blocking<T: Default + Send + 'static>(
        &self,
        query: impl FnOnce(&Connection) -> T + Send + 'static,
    ) -> T {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || query(&db.lock().unwrap()))
            .await
            .unwrap_or_default()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The window of a range query as SQLite integers.
fn bounds(from: u64, to: u64) -> [i64; 2] {
    [to_sql(from), to_sql(to)]
}

fn to_sql(ts: u64) -> i64 {
    i64::try_from(ts).unwrap_or(i64::MAX)
}

/// `events.db` in `dir`, or a database in memory if there is no `dir` or the
/// file is not usable. `events.jsonl` from `dir` is imported once.
fn open_database(dir: Option<&Path>) -> Connection {
    if let Some(dir) = dir {
        let path = dir.join("events.db");
        match open_file(&path) {
            Ok(mut conn) => {
                let jsonl = dir.join("events.jsonl");
                if let Err(e) = import_jsonl(&mut conn, &jsonl) {
                    tracing::warn!("Verlauf aus {} nicht übernommen: {e}", jsonl.display());
                }
                if let Err(e) = record_stop(&mut conn) {
                    tracing::warn!("Ende des letzten Laufs nicht verzeichnet: {e}");
                }
                return conn;
            }
            Err(e) => {
                tracing::warn!("Verlauf-Datenbank {} nicht nutzbar: {e}", path.display());
            }
        }
    }
    let conn = Connection::open_in_memory().expect("Speicher für den Verlauf");
    create_tables(&conn).expect("Verlauf im Speicher eingerichtet");
    conn
}

fn open_file(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    // Private from the first byte, like every file in DATA_DIR; SQLite gives
    // its WAL and shared-memory files the same mode.
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    create_tables(&conn)?;
    Ok(conn)
}

fn create_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
             ts INTEGER NOT NULL,
             kind TEXT NOT NULL,
             detail TEXT
         );
         CREATE INDEX IF NOT EXISTS events_ts ON events (ts);
         CREATE TABLE IF NOT EXISTS samples (
             ts INTEGER NOT NULL,
             watts REAL NOT NULL
         );
         CREATE INDEX IF NOT EXISTS samples_ts ON samples (ts);
         CREATE TABLE IF NOT EXISTS alive (
             id INTEGER PRIMARY KEY,
             ts INTEGER NOT NULL
         );",
    )
}

/// Moves the events of `events.jsonl` into the database in one transaction,
/// all or none, unless an earlier start did; lines that cannot be read, e.g.
/// of kinds this version does not know, are skipped. The file itself stays as
/// a backup and is never written again.
fn import_jsonl(conn: &mut Connection, path: &Path) -> rusqlite::Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= IMPORTED {
        return Ok(());
    }
    let transaction = conn.transaction()?;
    if let Ok(file) = fs::File::open(path) {
        let events = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Event>(&line).ok());
        for event in events {
            insert(&transaction, &event)?;
        }
    }
    transaction.pragma_update(None, "user_version", IMPORTED)?;
    // Dropping the transaction on an error above rolls it back.
    transaction.commit()
}

/// The last run's final `beat` as a `ProxyStopped` at its time, once.
fn record_stop(conn: &mut Connection) -> rusqlite::Result<()> {
    let transaction = conn.transaction()?;
    let last: Option<i64> = transaction
        .query_row("SELECT ts FROM alive WHERE id = 0", [], |row| row.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    if let Some(ts) = last {
        let stop = Event {
            ts: ts as u64,
            kind: EventKind::ProxyStopped,
            detail: None,
        };
        insert(&transaction, &stop)?;
        transaction.execute("DELETE FROM alive", [])?;
    }
    transaction.commit()
}

fn insert(conn: &Connection, event: &Event) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO events (ts, kind, detail) VALUES (?1, ?2, ?3)",
        params![event.ts as i64, kind_name(event.kind), event.detail],
    )?;
    Ok(())
}

/// The kind's snake_case name, as `events.jsonl` had it.
fn kind_name(kind: EventKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("an event kind serializes to its name"),
    }
}

/// The rows of `sql` as events; a row this version cannot read, e.g. of a kind
/// a newer version wrote, is skipped and the others are kept.
fn select(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<Event> {
    let rows = conn.prepare(sql).and_then(|mut statement| {
        statement
            .query_map(args, read_row)
            .map(|rows| rows.filter_map(Result::ok).collect())
    });
    rows.unwrap_or_else(|e| {
        tracing::warn!("Verlauf nicht gelesen: {e}");
        Vec::new()
    })
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let ts: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let kind = serde_json::from_value(serde_json::Value::String(name)).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Event {
        ts: ts as u64,
        kind,
        detail: row.get(2)?,
    })
}
