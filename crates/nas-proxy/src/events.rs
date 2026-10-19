//! History of wake and sleep events, for the web UI and the power estimate.
//! The Ereignisse live in a SQLite database in `DATA_DIR` (docs/adr/0003);
//! without one the store is in memory. All database calls run on a thread of
//! their own, never on the async runtime's threads.

use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub use strata_admin_ui::api::EventKind;

/// The most recent events `recent` and `latest_pc` return; the database keeps
/// all of them.
const KEEP_RECENT: usize = 1000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Unix time in seconds.
    pub ts: u64,
    pub kind: EventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

enum Command {
    Insert(Event, SyncSender<()>),
    Recent(SyncSender<Vec<Event>>),
    Range {
        from: u64,
        to: u64,
        reply: SyncSender<Vec<Event>>,
    },
    Quit,
}

pub struct EventLog {
    tx: SyncSender<Command>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl EventLog {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let files = data_dir.and_then(|dir| match fs::create_dir_all(&dir) {
            Ok(()) => Some((dir.join("events.db"), dir.join("events.jsonl"))),
            Err(e) => {
                tracing::warn!("DATA_DIR {} nicht nutzbar: {e}", dir.display());
                None
            }
        });
        // Opening and importing happen before `new` returns, so the database
        // exists like the loaded history did before.
        let (db, jsonl) = match &files {
            Some((db, jsonl)) => (Some(db.as_path()), Some(jsonl.as_path())),
            None => (None, None),
        };
        let conn = open_database(db, jsonl);
        let (tx, rx) = mpsc::sync_channel(64);
        let worker = std::thread::spawn(move || serve(conn, rx));
        Self {
            tx,
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Sends one command to the database thread and waits for its answer, so
    /// a recorded event is seen by every later call.
    fn ask<T: Default>(&self, make: impl FnOnce(SyncSender<T>) -> Command) -> T {
        let (reply, answer) = mpsc::sync_channel(1);
        if self.tx.send(make(reply)).is_err() {
            return T::default();
        }
        answer.recv().unwrap_or_default()
    }

    pub fn record(&self, kind: EventKind, detail: Option<String>) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        tracing::info!(?kind, detail = detail.as_deref(), "Ereignis");
        self.ask(|reply| Command::Insert(Event { ts, kind, detail }, reply));
    }

    /// The most recent events, oldest first.
    pub fn recent(&self) -> Vec<Event> {
        self.ask(Command::Recent)
    }

    /// The latest `n` events about the PC, newest first.
    pub fn latest_pc(&self, n: usize) -> Vec<Event> {
        let mut recent = self.recent();
        recent.reverse();
        recent
            .into_iter()
            .filter(|e| !e.kind.is_admin())
            .take(n)
            .collect()
    }

    /// Every event with `from <= ts <= to`, oldest first; the database holds
    /// them back to the first one.
    pub fn range(&self, from: u64, to: u64) -> Vec<Event> {
        self.ask(|reply| Command::Range { from, to, reply })
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

/// The database at `db`, or in memory if there is none or it cannot be
/// opened. On a first start with an empty database, `events.jsonl` is
/// imported into it.
fn open_database(db: Option<&Path>, jsonl: Option<&Path>) -> Connection {
    let conn = match db {
        Some(path) => match Connection::open(path) {
            Ok(conn) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Like the other files in DATA_DIR: only the owner reads it.
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
                }
                conn
            }
            Err(e) => {
                tracing::warn!("Verlauf-Datenbank {} nicht nutzbar: {e}", path.display());
                Connection::open_in_memory().expect("Speicher für den Verlauf")
            }
        },
        None => Connection::open_in_memory().expect("Speicher für den Verlauf"),
    };
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
             ts INTEGER NOT NULL,
             kind TEXT NOT NULL,
             detail TEXT
         );
         CREATE INDEX IF NOT EXISTS events_ts ON events (ts);",
    )
    .expect("Verlauf-Datenbank eingerichtet");
    if let Some(path) = jsonl {
        import_jsonl(&conn, path);
    }
    conn
}

/// On a first start with an empty database, the events of `events.jsonl` move
/// into it in one transaction; lines that cannot be read, e.g. of kinds this
/// version does not know, are skipped. The file itself stays as a backup and
/// is never written again.
fn import_jsonl(conn: &Connection, path: &Path) {
    let kept: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap_or(0);
    if kept > 0 {
        return;
    }
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let events: Vec<Event> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    if events.is_empty() {
        return;
    }
    if conn.execute("BEGIN", []).is_err() {
        return;
    }
    for event in &events {
        insert(conn, event);
    }
    if conn.execute("COMMIT", []).is_err() {
        tracing::warn!("Verlauf aus {} nicht übernommen", path.display());
    }
}

fn insert(conn: &Connection, event: &Event) {
    let kind = serde_json::to_string(&event.kind).expect("event kind serializes");
    let written = conn.execute(
        "INSERT INTO events (ts, kind, detail) VALUES (?1, ?2, ?3)",
        params![event.ts as i64, kind, event.detail],
    );
    if let Err(e) = written {
        tracing::warn!("Verlauf nicht geschrieben: {e}");
    }
}

fn serve(conn: Connection, rx: mpsc::Receiver<Command>) {
    while let Ok(command) = rx.recv() {
        match command {
            Command::Insert(event, reply) => {
                insert(&conn, &event);
                let _ = reply.send(());
            }
            Command::Recent(reply) => {
                let mut events = select(
                    &conn,
                    "SELECT ts, kind, detail FROM events ORDER BY rowid DESC LIMIT ?1",
                    &[&(KEEP_RECENT as i64)],
                );
                events.reverse();
                let _ = reply.send(events);
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

fn select(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<Event> {
    let Ok(mut statement) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map(args, read_row) else {
        return Vec::new();
    };
    rows.map_while(Result::ok).collect()
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
