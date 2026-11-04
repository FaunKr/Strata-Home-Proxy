//! The Transport check: where a JSON request body broke on its way to
//! Strata. The proxy checks each such body when it arrives and refuses one
//! that is broken; for a body Strata refuses as invalid JSON it keeps what
//! it sent and, in relay mode, what the PC service's relay counted
//! (`docs/pc-agent-api.md`). Rows, saved bodies (with `SAVE_BROKEN_BODIES`)
//! and the counts per side live in the history's database (docs/adr/0003).

use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    http::{HeaderMap, header},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use strata_admin_ui::api::{BrokenBodyRow, BrokenSide, RelayCount, TransportCheckView};
use strata_pc_protocol::{RELAY_BODY_PASSED, RELAY_BODY_RECEIVED, RELAY_BODY_SHA256_MATCH};

use crate::clock;

/// How many rows the tab keeps.
pub const ROWS: usize = 100;
/// How many saved bodies are kept.
pub const BODIES: usize = 10;

/// The tables `create_tables` of the history adds. `body` is a row's saved
/// body; `broken_body_counts` counts every row by side and is never trimmed.
pub const TABLES: &str = "CREATE TABLE IF NOT EXISTS broken_bodies (
     id INTEGER PRIMARY KEY,
     row TEXT NOT NULL,
     body BLOB
 );
 CREATE TABLE IF NOT EXISTS broken_body_counts (
     side TEXT PRIMARY KEY,
     count INTEGER NOT NULL
 );";

/// The request carries a body the proxy checks: not empty, sent as JSON.
pub fn checks(headers: &HeaderMap, body: &[u8]) -> bool {
    !body.is_empty()
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with("application/json"))
}

/// The `Content-Length` the client announced.
pub fn announced(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.trim().parse().ok())
}

/// Where a body's JSON breaks: the byte the parser stopped at and its
/// message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonBreak {
    pub at: u64,
    pub message: String,
}

/// Whether `body` is JSON; where it breaks if not. Read on the blocking
/// pool: a body can be many megabytes.
pub async fn check(body: Bytes) -> Option<JsonBreak> {
    tokio::task::spawn_blocking(move || json_break(&body))
        .await
        .unwrap_or(None)
}

fn json_break(body: &[u8]) -> Option<JsonBreak> {
    let e = serde_json::from_slice::<serde::de::IgnoredAny>(body).err()?;
    // At the end serde names the last byte; the text breaks after it.
    let at = if e.is_eof() {
        body.len() as u64
    } else {
        byte_of(body, e.line(), e.column())
    };
    Some(JsonBreak {
        at,
        message: e.to_string(),
    })
}

/// The byte offset of serde's `line` and `column` (both from 1; the column
/// counts bytes of its line, and 0 at a line's end before its first byte).
fn byte_of(body: &[u8], line: usize, column: usize) -> u64 {
    let start = if line <= 1 {
        0
    } else {
        body.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(line - 2)
            .map_or(body.len(), |(i, _)| i + 1)
    };
    (start + column.saturating_sub(1)).min(body.len()) as u64
}

/// The proxy's answer to a body that arrived broken.
pub fn refusal(at: u64, received: u64, announced: Option<u64>) -> String {
    let length = announced.map_or_else(
        || "no Content-Length".to_string(),
        |n| format!("Content-Length {n}"),
    );
    format!(
        "The request body arrived as invalid JSON at byte {at} of {received} received ({length}): the proxy did not pass it on."
    )
}

/// The SHA-256 of `body` in hex, as the relay compares it. On the blocking
/// pool.
pub async fn sha256(body: Bytes) -> String {
    tokio::task::spawn_blocking(move || {
        Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// What a request the proxy checked keeps for its row, until Strata answers.
#[derive(Clone, Debug)]
pub struct Checked {
    pub key: String,
    pub path: String,
    pub at: u64,
    pub announced: Option<u64>,
    pub received: u64,
}

/// Strata's message and the character it names, when its `400` is the
/// refusal of a body that is not JSON (`Expecting ',' delimiter: line 1
/// column 9 (char 8)`, Python's `json.loads` in `serve/server.py`).
pub fn strata_break(answer: &[u8]) -> Option<(String, u64)> {
    let value: serde_json::Value = serde_json::from_slice(answer).ok()?;
    let message = value.get("error")?.get("message")?.as_str()?;
    let (head, tail) = message.rsplit_once(" (char ")?;
    let at = tail.strip_suffix(')')?.parse().ok()?;
    let (_, place) = head.rsplit_once(": line ")?;
    let (line, column) = place.split_once(" column ")?;
    line.parse::<u64>().ok()?;
    column.parse::<u64>().ok()?;
    Some((message.to_string(), at))
}

/// What the relay's headers on Strata's answer say; `NotMeasured` without
/// them.
pub fn relay_count(headers: &HeaderMap) -> RelayCount {
    let number = |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.parse().ok() };
    match (number(RELAY_BODY_RECEIVED), number(RELAY_BODY_PASSED)) {
        (Some(received), Some(passed)) => RelayCount::Measured {
            received,
            passed,
            checksum_matches: headers
                .get(RELAY_BODY_SHA256_MATCH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| match v {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                }),
        },
        _ => RelayCount::NotMeasured,
    }
}

/// The side of a body Strata refused, `sent` bytes long.
pub fn side_of(relay: Option<RelayCount>, sent: u64) -> BrokenSide {
    match relay {
        Some(RelayCount::Measured {
            received,
            passed,
            checksum_matches,
        }) if received != sent || passed != sent || checksum_matches == Some(false) => {
            BrokenSide::Network
        }
        _ => BrokenSide::Strata,
    }
}

pub struct TransportCheck {
    db: Arc<Mutex<Connection>>,
}

impl TransportCheck {
    /// The check in `db`, which holds its tables. Without `saving` no body
    /// stays saved, also one from before a restart.
    pub fn new(db: Arc<Mutex<Connection>>, saving: bool) -> Self {
        let check = Self { db };
        if !saving && let Err(e) = check.db.lock().unwrap().execute(CLEAR, []) {
            tracing::warn!("saved bodies not deleted: {e}");
        }
        check
    }

    /// A body that arrived broken at `at`.
    pub async fn arrival(&self, checked: Checked, at: JsonBreak, body: Option<Bytes>) {
        let row = BrokenBodyRow {
            id: 0,
            at: checked.at,
            time: String::new(),
            key: checked.key,
            path: checked.path,
            side: BrokenSide::Arrival,
            announced: checked.announced,
            received: checked.received,
            break_at: at.at,
            message: at.message,
            sent_bytes: None,
            sent_chars: None,
            relay: None,
            saved: false,
        };
        self.add(row, body).await;
    }

    /// A body Strata refused as invalid JSON with `message`, naming
    /// character `at`; `sent` is the body as the proxy sent it.
    pub async fn strata(
        &self,
        checked: Checked,
        message: String,
        at: u64,
        sent: Bytes,
        relay: Option<RelayCount>,
        keep: bool,
    ) {
        let sent_bytes = sent.len() as u64;
        let body = sent.clone();
        let chars = tokio::task::spawn_blocking(move || {
            String::from_utf8_lossy(&body).chars().count() as u64
        })
        .await
        .unwrap_or_default();
        let row = BrokenBodyRow {
            id: 0,
            at: checked.at,
            time: String::new(),
            key: checked.key,
            path: checked.path,
            side: side_of(relay, sent_bytes),
            announced: checked.announced,
            received: checked.received,
            break_at: at,
            message,
            sent_bytes: Some(sent_bytes),
            sent_chars: Some(chars),
            relay,
            saved: false,
        };
        self.add(row, keep.then_some(sent)).await;
    }

    /// Writes `row` with its body, if saved, trims rows and bodies, and
    /// counts it; on the blocking pool, finished before the client's answer.
    async fn add(&self, row: BrokenBodyRow, body: Option<Bytes>) {
        let db = Arc::clone(&self.db);
        let written = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let json = serde_json::to_string(&row).map_err(|e| e.to_string())?;
            let db = db.lock().unwrap();
            db.execute(
                "INSERT INTO broken_bodies (row, body) VALUES (?1, ?2)",
                params![json, body.as_deref()],
            )
            .and_then(|_| {
                db.execute(
                    "DELETE FROM broken_bodies WHERE id NOT IN \
                     (SELECT id FROM broken_bodies ORDER BY id DESC LIMIT ?1)",
                    [ROWS as i64],
                )
            })
            .and_then(|_| {
                db.execute(
                    "UPDATE broken_bodies SET body = NULL WHERE body IS NOT NULL AND id NOT IN \
                     (SELECT id FROM broken_bodies WHERE body IS NOT NULL \
                      ORDER BY id DESC LIMIT ?1)",
                    [BODIES as i64],
                )
            })
            .and_then(|_| {
                db.execute(
                    "INSERT INTO broken_body_counts (side, count) VALUES (?1, 1) \
                     ON CONFLICT (side) DO UPDATE SET count = count + 1",
                    [row.side.as_str()],
                )
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = written {
            tracing::warn!("broken request body not recorded: {e}");
        }
    }

    /// The tab: the rows, newest first, with their local time.
    pub async fn view(&self, saving: bool) -> TransportCheckView {
        let db = Arc::clone(&self.db);
        let rows = tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            db.prepare("SELECT id, row, body IS NOT NULL FROM broken_bodies ORDER BY id DESC")
                .and_then(|mut statement| {
                    statement
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, bool>(2)?,
                            ))
                        })
                        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
                })
                .unwrap_or_else(|e| {
                    tracing::warn!("transport check not read: {e}");
                    Vec::new()
                })
        })
        .await
        .unwrap_or_default();
        let rows = rows
            .into_iter()
            // A row a newer version wrote and this one cannot read is skipped.
            .filter_map(|(id, json, saved)| {
                let mut row: BrokenBodyRow = serde_json::from_str(&json).ok()?;
                row.id = id as u64;
                row.saved = saved;
                row.time = clock::local_time_text(row.at);
                Some(row)
            })
            .collect();
        TransportCheckView { saving, rows }
    }

    /// The saved body of row `id`.
    pub async fn body(&self, id: u64) -> Option<Vec<u8>> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            db.lock()
                .unwrap()
                .query_row(
                    "SELECT body FROM broken_bodies WHERE id = ?1 AND body IS NOT NULL",
                    [id as i64],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .optional()
                .unwrap_or_else(|e| {
                    tracing::warn!("saved body not read: {e}");
                    None
                })
        })
        .await
        .unwrap_or_default()
    }

    /// Deletes every saved body; the rows stay.
    pub async fn delete_bodies(&self) -> Result<(), String> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.lock().unwrap().execute(CLEAR, []))
            .await
            .map_err(|e| e.to_string())?
            .map(|_| ())
            .map_err(|e| format!("The saved bodies were not deleted: {e}"))
    }

    /// [`Self::delete_bodies`] on this thread, for a setting just saved.
    pub fn delete_bodies_now(&self) -> Result<(), String> {
        self.db
            .lock()
            .unwrap()
            .execute(CLEAR, [])
            .map(|_| ())
            .map_err(|e| format!("The saved bodies were not deleted: {e}"))
    }

    /// Rows per side, over all time.
    pub async fn counts(&self) -> Vec<(BrokenSide, u64)> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            BrokenSide::ALL
                .iter()
                .map(|side| {
                    let count: i64 = db
                        .query_row(
                            "SELECT count FROM broken_body_counts WHERE side = ?1",
                            [side.as_str()],
                            |r| r.get(0),
                        )
                        .optional()
                        .ok()
                        .flatten()
                        .unwrap_or(0);
                    (*side, count.max(0) as u64)
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }
}

const CLEAR: &str = "UPDATE broken_bodies SET body = NULL WHERE body IS NOT NULL";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_body_breaks_at_its_end() {
        let body = br#"{"model": "x", "messages""#;
        let at = json_break(body).unwrap();
        assert_eq!(at.at, body.len() as u64, "{at:?}");
    }

    #[test]
    fn a_break_on_a_later_line_counts_the_lines_before() {
        let body = b"{\n  \"a\": 1,\n  \"b\" 2\n}";
        let at = json_break(body).unwrap();
        assert_eq!(&body[at.at as usize..at.at as usize + 1], b"2", "{at:?}");
    }

    #[test]
    fn strata_s_json_refusal_names_its_character() {
        let answer = br#"{"error": {"type": "invalid_request_error", "message": "Expecting ':' delimiter: line 1 column 294771 (char 294770)"}}"#;
        assert_eq!(
            strata_break(answer),
            Some((
                "Expecting ':' delimiter: line 1 column 294771 (char 294770)".to_string(),
                294770
            ))
        );
    }

    #[test]
    fn another_400_of_strata_is_no_json_refusal() {
        let answer = br#"{"error": {"type": "invalid_request_error", "message": "messages must be a non-empty list: send at least one message"}}"#;
        assert_eq!(strata_break(answer), None);
    }
}
