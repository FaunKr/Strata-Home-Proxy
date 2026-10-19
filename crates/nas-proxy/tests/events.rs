//! The store of the Verlauf: Ereignisse in SQLite unter `DATA_DIR`, ohne
//! `DATA_DIR` im Speicher. Ein vorhandenes `events.jsonl` wird einmal importiert.

use std::path::PathBuf;

use strata_nas_proxy::events::{EventKind, EventLog};

fn dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("strata-events-{}-{tag}", std::process::id()))
}

fn kinds(log: &EventLog) -> Vec<EventKind> {
    log.recent().into_iter().map(|e| e.kind).collect()
}

#[test]
fn events_survive_a_restart() {
    let d = dir("restart");
    let _ = std::fs::remove_dir_all(&d);
    {
        let log = EventLog::new(Some(d.clone()));
        log.record(EventKind::WakeSent, None);
        log.record(EventKind::Awake, Some("after 1.2 s".into()));
    }
    let log = EventLog::new(Some(d.clone()));
    assert_eq!(kinds(&log), vec![EventKind::WakeSent, EventKind::Awake]);
    let events = log.recent();
    assert_eq!(events[1].detail.as_deref(), Some("after 1.2 s"));
    // The database holds the history; `events.jsonl` is no longer written.
    assert!(d.join("events.db").exists());
    assert!(!d.join("events.jsonl").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn events_jsonl_is_imported_once() {
    let d = dir("import");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
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
    let events = log.recent();
    assert_eq!(events[1].detail.as_deref(), Some("benutzer inaktiv"));
    // The file stays as it was, as a backup.
    assert_eq!(
        std::fs::read_to_string(d.join("events.jsonl")).unwrap(),
        jsonl
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn range_reads_all_events_in_a_time_window() {
    let d = dir("range");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // Fixed times, so the window boundaries are exact.
    let jsonl = "{\"ts\":100,\"kind\":\"wake_sent\"}\n\
                 {\"ts\":200,\"kind\":\"sleep_accepted\"}\n\
                 {\"ts\":300,\"kind\":\"wake_sent\"}\n";
    std::fs::write(d.join("events.jsonl"), jsonl).unwrap();
    let log = EventLog::new(Some(d.clone()));
    let stamps = |events: Vec<strata_nas_proxy::events::Event>| -> Vec<u64> {
        events.into_iter().map(|e| e.ts).collect()
    };
    assert_eq!(stamps(log.range(0, u64::MAX).await), vec![100, 200, 300]);
    // The boundaries belong to the window.
    assert_eq!(stamps(log.range(100, 300).await), vec![100, 200, 300]);
    assert_eq!(stamps(log.range(150, 250).await), vec![200]);
    assert!(log.range(1000, 2000).await.is_empty());
    // A recorded event is in the range right away.
    log.record(EventKind::Awake, None);
    let all = log.range(0, u64::MAX).await;
    assert_eq!(all.last().map(|e| e.kind), Some(EventKind::Awake));
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

#[tokio::test]
async fn a_row_of_an_unknown_kind_hides_no_other_rows() {
    let d = dir("unknown-row");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // As a newer version would leave it: a kind this one does not know,
    // between two it knows.
    {
        let conn = rusqlite::Connection::open(d.join("events.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT);
             INSERT INTO events VALUES (100, '\"wake_sent\"', NULL);
             INSERT INTO events VALUES (150, '\"zeitreise\"', NULL);
             INSERT INTO events VALUES (200, '\"awake\"', NULL);",
        )
        .unwrap();
    }
    let log = EventLog::new(Some(d.clone()));
    assert_eq!(kinds(&log), vec![EventKind::WakeSent, EventKind::Awake]);
    let stamps: Vec<u64> = log
        .range(0, u64::MAX)
        .await
        .into_iter()
        .map(|e| e.ts)
        .collect();
    assert_eq!(stamps, vec![100, 200]);
    drop(log);
    std::fs::remove_dir_all(&d).unwrap();
}
