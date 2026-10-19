//! History of wake and sleep events, for the web UI and the power estimate.
//! The Ereignisse live in a SQLite database in `DATA_DIR` (docs/adr/0003);
//! without one the store is in memory. The database is written and queried on
//! a thread of its own, so no call parks a thread of the async runtime.

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub use strata_admin_ui::api::EventKind;

/// How many of the most recent events `recent` and `latest_pc` see; the
/// database keeps all of them.
const KEEP_IN_MEMORY: usize = 1000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Unix time in seconds.
    pub ts: u64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

enum Command {
    Insert(Event),
    Range {
        from: u64,
        to: u64,
        reply: oneshot::Sender<Vec<Event>>,
    },
    Quit,
}

pub struct EventLog {
    /// The latest events, for the overview, without asking the database.
    recent: Mutex<VecDeque<Event>>,
    tx: Sender<Command>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl EventLog {
    /// Opens the database and imports `events.jsonl` before returning, so call
    /// it at startup, not from an async task.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let dir = data_dir.and_then(|dir| match fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                tracing::warn!("DATA_DIR {} nicht nutzbar: {e}", dir.display());
                None
            }
        });
        let conn = open_database(dir.as_deref());
        // The history so far, so the overview shows it after a restart too.
        let mut recent = select(
            &conn,
            "SELECT ts, kind, detail FROM events ORDER BY rowid DESC LIMIT ?1",
            &[&(KEEP_IN_MEMORY as i64)],
        );
        recent.reverse();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || serve(conn, rx));
        Self {
            recent: Mutex::new(recent.into()),
            tx,
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Never waits for the database: the write is queued, and every later
    /// `range` sees it because the database thread works in order.
    pub fn record(&self, kind: EventKind, detail: Option<String>) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let event = Event { ts, kind, detail };
        tracing::info!(?kind, detail = event.detail.as_deref(), "Ereignis");
        {
            let mut recent = self.recent.lock().unwrap();
            if recent.len() == KEEP_IN_MEMORY {
                recent.pop_front();
            }
            recent.push_back(event.clone());
        }
        if self.tx.send(Command::Insert(event)).is_err() {
            tracing::warn!("Verlauf nicht geschrieben: Datenbank-Thread beendet");
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
            .filter(|e| !e.kind.is_admin())
            .take(n)
            .cloned()
            .collect()
    }

    /// Every event with `from <= ts <= to`, oldest first; the database holds
    /// them back to the first one.
    pub async fn range(&self, from: u64, to: u64) -> Vec<Event> {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Command::Range { from, to, reply }).is_err() {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }
}

impl Drop for EventLog {
    /// Waits for the queued writes, so a restart finds them.
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

/// `events.db` in `dir`, or a database in memory if there is no `dir` or the
/// file cannot be opened. On a first start with an empty database,
/// `events.jsonl` from `dir` is imported into it.
fn open_database(dir: Option<&Path>) -> Connection {
    let opened = dir.and_then(|dir| {
        let path = dir.join("events.db");
        match Connection::open(&path) {
            Ok(conn) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Like the other files in DATA_DIR: only the owner reads it.
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                }
                Some(conn)
            }
            Err(e) => {
                tracing::warn!("Verlauf-Datenbank {} nicht nutzbar: {e}", path.display());
                None
            }
        }
    });
    let mut conn =
        opened.unwrap_or_else(|| Connection::open_in_memory().expect("Speicher für den Verlauf"));
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
             ts INTEGER NOT NULL,
             kind TEXT NOT NULL,
             detail TEXT
         );
         CREATE INDEX IF NOT EXISTS events_ts ON events (ts);",
    )
    .expect("Verlauf-Datenbank eingerichtet");
    if let Some(dir) = dir {
        let path = dir.join("events.jsonl");
        if let Err(e) = import_jsonl(&mut conn, &path) {
            tracing::warn!("Verlauf aus {} nicht übernommen: {e}", path.display());
        }
    }
    conn
}

/// On a first start with an empty database, the events of `events.jsonl` move
/// into it in one transaction, all or none; lines that cannot be read, e.g. of
/// kinds this version does not know, are skipped. The file itself stays as a
/// backup and is never written again.
fn import_jsonl(conn: &mut Connection, path: &Path) -> rusqlite::Result<()> {
    let kept: i64 = conn.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?;
    if kept > 0 {
        return Ok(());
    }
    let Ok(file) = fs::File::open(path) else {
        return Ok(());
    };
    let events = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Event>(&line).ok());
    // Dropping the transaction on an error rolls it back.
    let tx = conn.transaction()?;
    for event in events {
        insert(&tx, &event)?;
    }
    tx.commit()
}

fn insert(conn: &Connection, event: &Event) -> rusqlite::Result<()> {
    let kind = serde_json::to_string(&event.kind).expect("event kind serializes");
    conn.execute(
        "INSERT INTO events (ts, kind, detail) VALUES (?1, ?2, ?3)",
        params![event.ts as i64, kind, event.detail],
    )?;
    Ok(())
}

fn serve(conn: Connection, rx: mpsc::Receiver<Command>) {
    while let Ok(command) = rx.recv() {
        match command {
            Command::Insert(event) => {
                if let Err(e) = insert(&conn, &event) {
                    tracing::warn!("Verlauf nicht geschrieben: {e}");
                }
            }
            Command::Range { from, to, reply } => {
                let events = select(
                    &conn,
                    "SELECT ts, kind, detail FROM events WHERE ts BETWEEN ?1 AND ?2 ORDER BY rowid",
                    &[
                        &i64::try_from(from).unwrap_or(i64::MAX),
                        &i64::try_from(to).unwrap_or(i64::MAX),
                    ],
                );
                let _ = reply.send(events);
            }
            Command::Quit => break,
        }
    }
}

/// The rows of `sql` as events; a row this version cannot read, e.g. of a kind
/// a newer version wrote, is skipped and the others are kept.
fn select(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<Event> {
    let Ok(mut statement) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map(args, read_row) else {
        return Vec::new();
    };
    rows.filter_map(Result::ok).collect()
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let ts: i64 = row.get(0)?;
    let kind: String = row.get(1)?;
    let Ok(kind) = serde_json::from_str(&kind) else {
        return Err(rusqlite::Error::InvalidParameterName("kind".into()));
    };
    Ok(Event {
        ts: ts as u64,
        kind,
        detail: row.get(2)?,
    })
}
