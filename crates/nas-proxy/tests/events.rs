//! The store of the history: events and the power of the power plug in
//! SQLite in `DATA_DIR`, in memory without one. An existing `events.jsonl`
//! is imported once.

use std::path::{Path, PathBuf};

use strata_nas_proxy::events::{Event, EventKind, History, Sample};

/// An empty directory of its own for each test.
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("strata-events-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kinds(log: &History) -> Vec<EventKind> {
    log.recent().into_iter().map(|e| e.kind).collect()
}

fn stamps(events: Vec<Event>) -> Vec<u64> {
    events.into_iter().map(|e| e.ts).collect()
}

/// A database as an earlier start or another version left it.
fn database(dir: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(dir.join("events.db")).unwrap();
    conn.execute_batch(sql).unwrap();
}

#[test]
fn events_survive_a_restart() {
    let d = fresh_dir("restart");
    {
        let log = History::new(Some(d.clone()));
        log.record(EventKind::WakeSent, None);
        log.record(EventKind::Awake, Some("after 1.2 s".into()));
        // The first log is still open, as when the proxy is killed.
        let restarted = History::new(Some(d.clone()));
        assert_eq!(
            kinds(&restarted),
            vec![EventKind::WakeSent, EventKind::Awake]
        );
    }
    let log = History::new(Some(d.clone()));
    assert_eq!(kinds(&log), vec![EventKind::WakeSent, EventKind::Awake]);
    assert_eq!(log.recent()[1].detail.as_deref(), Some("after 1.2 s"));
    // The database holds the history; `events.jsonl` is no longer written.
    assert!(d.join("events.db").exists());
    assert!(!d.join("events.jsonl").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[cfg(unix)]
#[test]
fn only_the_owner_can_read_the_database() {
    use std::os::unix::fs::PermissionsExt;
    let d = fresh_dir("mode");
    let log = History::new(Some(d.clone()));
    log.record(EventKind::WakeSent, None);
    for entry in std::fs::read_dir(&d).unwrap() {
        let entry = entry.unwrap();
        let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", entry.path().display());
    }
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn events_jsonl_is_imported_once() {
    let d = fresh_dir("import");
    let jsonl = "{\"ts\":100,\"kind\":\"wake_sent\"}\n\
                 broken line {\n\
                 {\"ts\":150,\"kind\":\"time_travel\"}\n\
                 {\"ts\":200,\"kind\":\"sleep_accepted\",\"detail\":\"user inactive\"}\n";
    std::fs::write(d.join("events.jsonl"), jsonl).unwrap();
    // Unreadable lines and unknown kinds are skipped, the rest imported.
    let expected = vec![EventKind::WakeSent, EventKind::SleepAccepted];
    {
        let log = History::new(Some(d.clone()));
        assert_eq!(kinds(&log), expected);
    }
    // A second start does not import the file again.
    let log = History::new(Some(d.clone()));
    assert_eq!(kinds(&log), expected);
    assert_eq!(log.recent()[1].detail.as_deref(), Some("user inactive"));
    // The file stays as it was, as a backup.
    assert_eq!(
        std::fs::read_to_string(d.join("events.jsonl")).unwrap(),
        jsonl
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_import_that_did_not_happen_is_made_up_later_in_order() {
    let d = fresh_dir("late-import");
    // An earlier start recorded an event but did not import the file.
    database(
        &d,
        "CREATE TABLE events (ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT);
         INSERT INTO events VALUES (500, 'awake', NULL);",
    );
    std::fs::write(
        d.join("events.jsonl"),
        "{\"ts\":100,\"kind\":\"wake_sent\"}\n{\"ts\":200,\"kind\":\"sleep_accepted\"}\n",
    )
    .unwrap();
    let log = History::new(Some(d.clone()));
    assert_eq!(
        stamps(log.recent()),
        vec![100, 200, 500],
        "imported events sort by time"
    );
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn range_reads_all_events_in_a_time_window() {
    let d = fresh_dir("range");
    // Fixed times, so the window boundaries are exact.
    let jsonl = "{\"ts\":100,\"kind\":\"wake_sent\"}\n\
                 {\"ts\":200,\"kind\":\"sleep_accepted\"}\n\
                 {\"ts\":300,\"kind\":\"wake_sent\"}\n";
    std::fs::write(d.join("events.jsonl"), jsonl).unwrap();
    let log = History::new(Some(d.clone()));
    assert_eq!(stamps(log.range(0, u64::MAX).await), vec![100, 200, 300]);
    // The boundaries belong to the window.
    assert_eq!(stamps(log.range(100, 300).await), vec![100, 200, 300]);
    assert_eq!(stamps(log.range(150, 250).await), vec![200]);
    assert!(log.range(1000, 2000).await.is_empty());
    // A recorded event is in the range right away.
    log.record(EventKind::Awake, None);
    let all = log.range(0, u64::MAX).await;
    assert_eq!(all.last().map(|e| e.kind), Some(EventKind::Awake));
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn a_row_of_an_unknown_kind_hides_no_other_rows() {
    let d = fresh_dir("unknown-row");
    // As a newer version would leave it: a kind this one does not know,
    // between two it knows.
    database(
        &d,
        "CREATE TABLE events (ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT);
         INSERT INTO events VALUES (100, 'wake_sent', NULL);
         INSERT INTO events VALUES (150, 'time_travel', NULL);
         INSERT INTO events VALUES (200, 'awake', NULL);",
    );
    let log = History::new(Some(d.clone()));
    assert_eq!(kinds(&log), vec![EventKind::WakeSent, EventKind::Awake]);
    assert_eq!(stamps(log.range(0, u64::MAX).await), vec![100, 200]);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn last_before_finds_the_latest_event_of_some_kinds() {
    let d = fresh_dir("last-before");
    database(
        &d,
        "CREATE TABLE events (ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT);
         INSERT INTO events VALUES (100, 'pc_seen_asleep', NULL);
         INSERT INTO events VALUES (200, 'awake', NULL);
         INSERT INTO events VALUES (250, 'password_changed', NULL);
         INSERT INTO events VALUES (300, 'pc_seen_asleep', NULL);",
    );
    let log = History::new(Some(d.clone()));
    let kinds = [EventKind::Awake, EventKind::PcSeenAsleep];
    let found = |e: Option<Event>| e.map(|e| (e.ts, e.kind));
    assert_eq!(
        found(log.last_before(300, &kinds).await),
        Some((200, EventKind::Awake))
    );
    assert_eq!(
        found(log.last_before(1000, &kinds).await),
        Some((300, EventKind::PcSeenAsleep))
    );
    assert_eq!(found(log.last_before(100, &kinds).await), None);
    assert_eq!(log.first_ts().await, Some(100));
    assert_eq!(History::new(None).first_ts().await, None);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_unusable_database_falls_back_to_memory() {
    let d = fresh_dir("broken");
    std::fs::write(
        d.join("events.db"),
        "no database, only text, long enough for a header",
    )
    .unwrap();
    let log = History::new(Some(d.clone()));
    log.record(EventKind::WakeSent, None);
    assert_eq!(kinds(&log), vec![EventKind::WakeSent]);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn no_data_dir_keeps_the_store_in_memory() {
    {
        let log = History::new(None);
        log.record(EventKind::WakeSent, None);
        assert_eq!(kinds(&log), vec![EventKind::WakeSent]);
    }
    let log = History::new(None);
    assert!(log.recent().is_empty());
}

#[test]
fn the_next_start_records_when_the_proxy_was_last_seen_running() {
    let d = fresh_dir("beat");
    {
        let log = History::new(Some(d.clone()));
        log.record(EventKind::WakeSent, None);
        log.beat();
    }
    let log = History::new(Some(d.clone()));
    let recent = log.recent();
    assert_eq!(
        kinds(&log),
        vec![EventKind::WakeSent, EventKind::ProxyStopped]
    );
    assert!(recent[1].ts >= recent[0].ts);
    drop(log);
    // An event after the last beat moves the stop to it.
    let log = History::new(Some(d.clone()));
    log.beat();
    log.record(EventKind::Awake, None);
    let awake = log.recent().last().unwrap().ts;
    drop(log);
    let log = History::new(Some(d.clone()));
    let recent = log.recent();
    let stop = recent.last().unwrap();
    assert_eq!(stop.kind, EventKind::ProxyStopped);
    assert!(stop.ts >= awake);
    drop(log);
    // Once only: a start after a run without a beat records no stop.
    let log = History::new(Some(d.clone()));
    assert_eq!(
        kinds(&log),
        vec![
            EventKind::WakeSent,
            EventKind::ProxyStopped,
            EventKind::Awake,
            EventKind::ProxyStopped
        ]
    );
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn latest_pc_skips_events_about_the_web_ui() {
    let log = History::new(None);
    log.record(EventKind::WakeSent, None);
    log.record(EventKind::PasswordChanged, None);
    log.record(EventKind::Awake, None);
    // And the marks the proxy sets for the history on its own.
    log.record(EventKind::PcSeenAwake, None);
    log.record(EventKind::ProxyStarted, None);
    let pc: Vec<EventKind> = log.latest_pc(10).into_iter().map(|e| e.kind).collect();
    // Newest first, as the overview shows them.
    assert_eq!(pc, vec![EventKind::Awake, EventKind::WakeSent]);
}

fn sample(ts: u64, watts: f64) -> Sample {
    Sample { ts, watts }
}

fn sample_stamps(samples: Vec<Sample>) -> Vec<u64> {
    samples.into_iter().map(|s| s.ts).collect()
}

#[tokio::test]
async fn samples_survive_a_restart() {
    let d = fresh_dir("samples-restart");
    {
        let history = History::new(Some(d.clone()));
        history.record_sample(sample(100, 87.4));
        history.record_sample(sample(160, 1.5));
        // Still open, as when the proxy is killed.
        let restarted = History::new(Some(d.clone()));
        assert_eq!(
            restarted.samples(0, u64::MAX).await,
            vec![sample(100, 87.4), sample(160, 1.5)]
        );
    }
    let history = History::new(Some(d.clone()));
    assert_eq!(
        history.samples(0, u64::MAX).await,
        vec![sample(100, 87.4), sample(160, 1.5)]
    );
    // A sample is no event.
    assert!(history.recent().is_empty());
    assert!(history.range(0, u64::MAX).await.is_empty());
    drop(history);
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn samples_reads_a_time_window_with_its_bounds() {
    let history = History::new(None);
    // Recorded out of order; read back by time.
    for (ts, watts) in [(300, 3.0), (100, 1.0), (200, 2.0)] {
        history.record_sample(sample(ts, watts));
    }
    assert_eq!(
        sample_stamps(history.samples(0, u64::MAX).await),
        vec![100, 200, 300]
    );
    // The boundaries belong to the window.
    assert_eq!(
        sample_stamps(history.samples(100, 300).await),
        vec![100, 200, 300]
    );
    assert_eq!(sample_stamps(history.samples(101, 299).await), vec![200]);
    assert!(history.samples(1000, 2000).await.is_empty());
}

#[tokio::test]
async fn hourly_sums_the_samples_of_each_hour() {
    let history = History::new(None);
    // Two minutes of one hour, one of the next.
    for (ts, watts) in [(3600, 10.0), (3610, 20.0), (3660, 30.0), (7200, 5.0)] {
        history.record_sample(sample(ts, watts));
    }
    let hours = history.hourly(0, u64::MAX, 60).await;
    assert_eq!(hours.len(), 2);
    assert_eq!((hours[0].hour, hours[0].slots), (3600, 2));
    assert_eq!(hours[0].mean_watts, 20.0);
    assert_eq!((hours[1].hour, hours[1].slots), (7200, 1));
    assert!(history.hourly(8000, 9000, 60).await.is_empty());
}

#[tokio::test]
async fn an_events_database_from_before_gets_the_samples_table() {
    let d = fresh_dir("samples-upgrade");
    // As the version without samples left it.
    database(
        &d,
        "CREATE TABLE events (ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT);
         INSERT INTO events VALUES (100, 'wake_sent', NULL);
         PRAGMA user_version = 1;",
    );
    let history = History::new(Some(d.clone()));
    history.record_sample(sample(200, 42.0));
    assert_eq!(history.samples(0, u64::MAX).await, vec![sample(200, 42.0)]);
    assert_eq!(kinds(&history), vec![EventKind::WakeSent]);
    drop(history);
    std::fs::remove_dir_all(&d).unwrap();
}
