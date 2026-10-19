//! The store of the Verlauf: Ereignisse in SQLite in `DATA_DIR`, in memory
//! without one. An existing `events.jsonl` is imported once.

use std::path::{Path, PathBuf};

use strata_nas_proxy::events::{Event, EventKind, EventLog};

/// An empty directory of its own for each test.
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("strata-events-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kinds(log: &EventLog) -> Vec<EventKind> {
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
        let log = EventLog::new(Some(d.clone()));
        log.record(EventKind::WakeSent, None);
        log.record(EventKind::Awake, Some("after 1.2 s".into()));
        // The first log is still open, as when the proxy is killed.
        let restarted = EventLog::new(Some(d.clone()));
        assert_eq!(
            kinds(&restarted),
            vec![EventKind::WakeSent, EventKind::Awake]
        );
    }
    let log = EventLog::new(Some(d.clone()));
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
    let log = EventLog::new(Some(d.clone()));
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
                 kaputte Zeile {\n\
                 {\"ts\":150,\"kind\":\"zeitreise\"}\n\
                 {\"ts\":200,\"kind\":\"sleep_accepted\",\"detail\":\"benutzer inaktiv\"}\n";
    std::fs::write(d.join("events.jsonl"), jsonl).unwrap();
    // Unreadable lines and unknown kinds are skipped, the rest imported.
    let expected = vec![EventKind::WakeSent, EventKind::SleepAccepted];
    {
        let log = EventLog::new(Some(d.clone()));
        assert_eq!(kinds(&log), expected);
    }
    // A second start does not import the file again.
    let log = EventLog::new(Some(d.clone()));
    assert_eq!(kinds(&log), expected);
    assert_eq!(log.recent()[1].detail.as_deref(), Some("benutzer inaktiv"));
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
    let log = EventLog::new(Some(d.clone()));
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
    let log = EventLog::new(Some(d.clone()));
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
         INSERT INTO events VALUES (150, 'zeitreise', NULL);
         INSERT INTO events VALUES (200, 'awake', NULL);",
    );
    let log = EventLog::new(Some(d.clone()));
    assert_eq!(kinds(&log), vec![EventKind::WakeSent, EventKind::Awake]);
    assert_eq!(stamps(log.range(0, u64::MAX).await), vec![100, 200]);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_unusable_database_falls_back_to_memory() {
    let d = fresh_dir("broken");
    std::fs::write(
        d.join("events.db"),
        "keine Datenbank, nur Text, lang genug für einen Kopf",
    )
    .unwrap();
    let log = EventLog::new(Some(d.clone()));
    log.record(EventKind::WakeSent, None);
    assert_eq!(kinds(&log), vec![EventKind::WakeSent]);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn no_data_dir_keeps_the_store_in_memory() {
    {
        let log = EventLog::new(None);
        log.record(EventKind::WakeSent, None);
        assert_eq!(kinds(&log), vec![EventKind::WakeSent]);
    }
    let log = EventLog::new(None);
    assert!(log.recent().is_empty());
}

#[test]
fn latest_pc_skips_events_about_the_web_ui() {
    let log = EventLog::new(None);
    log.record(EventKind::WakeSent, None);
    log.record(EventKind::PasswordChanged, None);
    log.record(EventKind::Awake, None);
    let pc: Vec<EventKind> = log.latest_pc(10).into_iter().map(|e| e.kind).collect();
    // Newest first, as the overview shows them.
    assert_eq!(pc, vec![EventKind::Awake, EventKind::WakeSent]);
}
