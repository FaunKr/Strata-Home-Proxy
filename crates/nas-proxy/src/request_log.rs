//! The request log: the last requests through the proxy that made the model
//! work, with numbers and labels only (`request_meter`). Finished rows live
//! in the history's database (docs/adr/0003), trimmed to the newest
//! `LIMIT`, so they survive a restart; running rows live in memory. The web
//! UI follows both live.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use rusqlite::{Connection, params};
use strata_admin_ui::api::{
    RequestLogMessage, RequestOutcome, RequestRefusal, RequestRow, RequestState,
};
use tokio::sync::broadcast;

use crate::{clock, request_meter::Meter, state::AppState};

/// How many finished requests the log keeps.
pub const LIMIT: usize = 1000;

pub struct RequestLog {
    db: Arc<Mutex<Connection>>,
    limit: AtomicUsize,
    next_id: AtomicU64,
    /// Rows not finished yet, by id.
    running: Mutex<BTreeMap<u64, RequestRow>>,
    changes: broadcast::Sender<RequestRow>,
}

impl RequestLog {
    /// The log in `db`, which holds the table `requests`.
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        let last: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT IFNULL(MAX(id), 0) FROM requests", [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|e| {
                tracing::warn!("request log not read: {e}");
                0
            });
        Self {
            db,
            limit: AtomicUsize::new(LIMIT),
            next_id: AtomicU64::new(last.max(0) as u64 + 1),
            running: Mutex::default(),
            changes: broadcast::channel(256).0,
        }
    }

    /// For tests: keeps `limit` finished requests instead of `LIMIT`.
    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit, Ordering::SeqCst);
    }

    /// Starts the row of a request that reached the proxy at `arrived`, or none
    /// when requests to `path` make no row.
    pub fn begin(
        state: &Arc<AppState>,
        key: &str,
        arrived: Instant,
        path: &str,
        body: &[u8],
    ) -> Option<Recording> {
        let meter = Meter::for_request(path, body)?;
        let log = &state.requests;
        Some(Recording {
            state: Arc::clone(state),
            meter,
            id: log.next_id.fetch_add(1, Ordering::SeqCst),
            at: clock::unix_now(),
            key: key.to_string(),
            arrived,
            sent: None,
            status: None,
            answer_bytes: Some(Vec::new()),
            done: false,
        })
    }

    /// Logs a request to `path` the proxy answered itself, before it read
    /// the body.
    pub fn refused(
        state: &Arc<AppState>,
        key: &str,
        arrived: Instant,
        path: &str,
        reason: RequestRefusal,
    ) {
        if let Some(rec) = Self::begin(state, key, arrived, path, b"") {
            rec.refused(reason);
        }
    }

    /// A row that has not finished, new or changed.
    fn show(&self, row: RequestRow) {
        self.running.lock().unwrap().insert(row.id, row.clone());
        let _ = self.changes.send(row);
    }

    /// Written at once, like an event, so a killed proxy loses nothing.
    fn finish(&self, row: RequestRow) {
        self.running.lock().unwrap().remove(&row.id);
        let limit = self.limit.load(Ordering::SeqCst) as i64;
        let written = serde_json::to_string(&row)
            .map_err(|e| e.to_string())
            .and_then(|json| {
                let db = self.db.lock().unwrap();
                db.execute(
                    "INSERT INTO requests (id, row) VALUES (?1, ?2)",
                    params![row.id as i64, json],
                )
                .and_then(|_| {
                    db.execute(
                        "DELETE FROM requests WHERE seq NOT IN \
                         (SELECT seq FROM requests ORDER BY seq DESC LIMIT ?1)",
                        [limit],
                    )
                })
                .map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            tracing::warn!("request not logged: {e}");
        }
        let _ = self.changes.send(row);
    }

    /// The finished rows, newest first. Read on a blocking thread.
    async fn rows(&self) -> Vec<RequestRow> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let db = db.lock().unwrap();
            let rows = db
                .prepare("SELECT row FROM requests ORDER BY id DESC")
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| row.get::<_, String>(0))
                        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
                });
            rows.unwrap_or_else(|e| {
                tracing::warn!("request log not read: {e}");
                Vec::new()
            })
            .iter()
            // A row a newer version wrote and this one cannot read is skipped.
            .filter_map(|json| serde_json::from_str(json).ok())
            .collect()
        })
        .await
        .unwrap_or_default()
    }
}

/// The table `create_tables` of the history adds.
/// `seq` counts rows as they finish, so the trim keeps the rows that
/// finished last; `id` counts requests as they arrive.
pub const TABLE: &str = "CREATE TABLE IF NOT EXISTS requests (
     seq INTEGER PRIMARY KEY,
     id INTEGER NOT NULL,
     row TEXT NOT NULL
 );";

/// The log for one page: all of it first, running rows on top, then each
/// row as it changes.
pub fn watch(state: &Arc<AppState>) -> BoxStream<'static, RequestLogMessage> {
    let state = Arc::clone(state);
    // Subscribed before reading, so no row falls between; one sent twice
    // only takes its own place again.
    let rx = state.requests.changes.subscribe();
    let first = futures_util::stream::once(async move {
        let finished = state.requests.rows().await;
        let running = state.requests.running.lock().unwrap().clone();
        // A row that finished while the log was read is in both.
        let mut rows: Vec<RequestRow> = running
            .into_values()
            .rev()
            .filter(|r| !finished.iter().any(|f| f.id == r.id))
            .collect();
        rows.extend(finished);
        rows.iter_mut().for_each(with_time);
        RequestLogMessage::All { rows }
    });
    let changes = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(mut row) => {
                    with_time(&mut row);
                    return Some((RequestLogMessage::Row { row: Box::new(row) }, rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    first.chain(changes).boxed()
}

fn with_time(row: &mut RequestRow) {
    row.time = clock::local_time_text(row.at);
}

/// One request on its way: the meter and what the proxy itself clocks. A
/// recording dropped without an end is a client that went away, or a
/// request given up as lost.
pub struct Recording {
    state: Arc<AppState>,
    meter: Meter,
    id: u64,
    at: u64,
    key: String,
    arrived: Instant,
    sent: Option<Instant>,
    status: Option<u16>,
    /// A whole answer's bytes, until it gets too large to read.
    answer_bytes: Option<Vec<u8>>,
    done: bool,
}

/// A whole answer larger than this is passed on without being read.
const WHOLE_LIMIT: usize = 8 << 20;

impl Recording {
    /// The request waits for the PC to wake, or for a run over all models.
    pub fn waiting(&self) {
        self.show(RequestState::WakingPc);
    }

    /// The request went to Strata at `at`.
    pub fn sent(&mut self, at: Instant) {
        self.sent = Some(at);
        self.show(RequestState::Running);
    }

    fn show(&self, state: RequestState) {
        let mut row = self.row();
        row.state = state;
        self.state.requests.show(row);
    }

    /// The proxy answered the request itself.
    pub fn refused(self, reason: RequestRefusal) {
        self.finish(RequestOutcome::Refused { reason });
    }

    /// Strata's answer status.
    pub fn answered(&mut self, status: u16) {
        self.status = Some(status);
    }

    /// Strata's whole answer, read at once.
    pub fn whole(&mut self, body: &[u8]) {
        self.meter.whole(body);
    }

    /// Ends the row: `Done` becomes what the answer says (cut off, an error
    /// status).
    pub fn finish(mut self, outcome: RequestOutcome) {
        self.end(outcome);
    }

    /// Passes an answer's body through and reads it on the way: an event
    /// stream piece by piece, anything else as a whole at its end. A body of
    /// a known `length` ends with its last byte: the client side stops
    /// asking for more then.
    pub fn metered<S, E>(
        mut self,
        body: S,
        events: bool,
        length: Option<u64>,
    ) -> impl Stream<Item = Result<Bytes, E>> + Send
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: Send + 'static,
    {
        let mut left = length.filter(|_| !events);
        if left == Some(0) {
            self.answer_complete();
        }
        futures_util::stream::unfold(Some((Box::pin(body), self)), move |held| async move {
            let (mut body, mut rec) = held?;
            match body.next().await {
                Some(Ok(bytes)) => {
                    rec.read(&bytes, events);
                    if let Some(left) = &mut left {
                        *left = left.saturating_sub(bytes.len() as u64);
                        if *left == 0 {
                            rec.answer_complete();
                        }
                    }
                    Some((Ok(bytes), Some((body, rec))))
                }
                Some(Err(e)) => {
                    rec.end(RequestOutcome::NoAnswer);
                    Some((Err(e), None))
                }
                None => {
                    rec.answer_complete();
                    None
                }
            }
        })
    }

    /// The answer came in full.
    fn answer_complete(&mut self) {
        if let Some(whole) = self.answer_bytes.take() {
            self.meter.whole(&whole);
        }
        self.end(RequestOutcome::Done);
    }

    fn read(&mut self, bytes: &[u8], events: bool) {
        if self.done {
            return;
        }
        if events {
            let at = self.sent.map(|s| s.elapsed()).unwrap_or_default();
            self.meter.chunk(bytes, at);
        } else if let Some(whole) = &mut self.answer_bytes {
            if whole.len() + bytes.len() > WHOLE_LIMIT {
                self.answer_bytes = None;
            } else {
                whole.extend_from_slice(bytes);
            }
        }
    }

    fn end(&mut self, outcome: RequestOutcome) {
        if self.done {
            return;
        }
        self.done = true;
        let mut row = self.row();
        let sent = self.sent.unwrap_or_else(Instant::now);
        row.wait_ms = sent.saturating_duration_since(self.arrived).as_millis() as u64;
        row.duration_ms = self.arrived.elapsed().as_millis() as u64;
        row.outcome = match (outcome, self.status) {
            (RequestOutcome::Done, Some(status)) if !(200..300).contains(&status) => {
                RequestOutcome::StrataError { status }
            }
            (RequestOutcome::Done, _) => row.outcome,
            (outcome, _) => outcome,
        };
        if matches!(
            row.outcome,
            RequestOutcome::CancelledByClient | RequestOutcome::Lost | RequestOutcome::NoAnswer
        ) {
            row = row.without_counts();
        }
        self.state.requests.finish(row);
    }

    /// What the meter knows, with the request's own fields.
    fn row(&self) -> RequestRow {
        RequestRow {
            id: self.id,
            at: self.at,
            key: self.key.clone(),
            ..self.meter.row()
        }
    }
}

impl Drop for Recording {
    /// Dropped without an end: the client went away, or the proxy gave the
    /// request up and dropped its answer.
    fn drop(&mut self) {
        let lost = self.sent.is_some_and(|sent| self.state.was_given_up(sent));
        self.end(if lost {
            RequestOutcome::Lost
        } else {
            RequestOutcome::CancelledByClient
        });
    }
}
