//! The Strata output: what Strata writes to its console, as `GET /strata-log`
//! hands it on.

use crate::common;

use std::time::Duration;

use common::*;
use serde_json::Value;

/// `GET /strata-log` read like the proxy reads it.
struct Log {
    body: std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: String,
    /// The reason of the last notice read: the service sends a notice only
    /// when it changes.
    notice: Option<String>,
}

impl Log {
    async fn open(s: &Setup) -> Self {
        let resp = client()
            .get(format!("{}/strata-log", s.url))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        Self {
            body: Box::pin(resp.bytes_stream()),
            buf: String::new(),
            notice: None,
        }
    }

    async fn next(&mut self) -> Value {
        use futures_util::StreamExt;
        let read = async {
            loop {
                if let Some(end) = self.buf.find("\n\n") {
                    let event: String = self.buf.drain(..end + 2).collect();
                    if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data:")) {
                        return serde_json::from_str(data.trim()).unwrap();
                    }
                    continue;
                }
                let chunk = self.body.next().await.expect("stream ended").unwrap();
                self.buf.push_str(&String::from_utf8_lossy(&chunk));
            }
        };
        let msg: Value = tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("no message");
        if msg["kind"] == "notice" {
            self.notice = msg["reason"].as_str().map(String::from);
        }
        msg
    }

    /// The next messages until `check` holds for one; returns all of them.
    async fn until(&mut self, check: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let msg = self.next().await;
            let done = check(&msg);
            seen.push(msg);
            if done {
                return seen;
            }
        }
    }
}

fn texts(msgs: &[Value]) -> Vec<String> {
    msgs.iter()
        .filter(|m| m["kind"] == "line")
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect()
}

fn strata_prints(s: &Setup, lines: &[&str]) {
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            pause: Duration::from_millis(20),
            hang: true,
            ..Default::default()
        },
    );
}

/// Ends Strata through gaming mode and lets the service start it again.
async fn restart(s: &Setup) {
    post(s, "/gaming", serde_json::json!({"enabled": true})).await;
    post(s, "/gaming", serde_json::json!({"enabled": false})).await;
}

#[tokio::test]
async fn output_of_a_started_strata_arrives_live() {
    let s = setup("output-live").await;
    strata_prints(&s, &["Loading model", "Model loaded"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    let seen = log.until(|m| m["text"] == "Model loaded").await;
    assert_eq!(texts(&seen), ["Loading model", "Model loaded"]);
    assert!(seen.iter().any(|m| m["kind"] == "started"), "{seen:?}");

    // Opened later, the same lines come first.
    let mut later = Log::open(&s).await;
    let seen = later.until(|m| m["text"] == "Model loaded").await;
    assert_eq!(texts(&seen), ["Loading model", "Model loaded"]);
}

#[tokio::test]
async fn strata_log_needs_the_token() {
    let s = setup("output-token").await;
    let resp = client()
        .get(format!("{}/strata-log", s.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn strata_log_file_holds_the_last_run() {
    let s = setup("output-file").await;
    strata_prints(&s, &["first run"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "first run").await;
    strata_prints(&s, &["second run"]);
    restart(&s).await;
    log.until(|m| m["text"] == "second run").await;
    let file = std::fs::read_to_string(s.dir.join("strata.log")).unwrap();
    assert_eq!(file, "second run\n");
}

#[tokio::test]
async fn progress_rewriting_its_line_stays_one_line() {
    let s = setup("output-progress").await;
    // A line ended by \r is rewritten by the next one, as in a console.
    strata_prints(&s, &["loading 10%\r", "loading 100%\r", "done"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    let seen = log.until(|m| m["text"] == "done").await;
    let kinds: Vec<_> = seen
        .iter()
        .filter(|m| m["kind"] == "line" || m["kind"] == "replace")
        .map(|m| {
            format!(
                "{} {}",
                m["kind"].as_str().unwrap(),
                m["text"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        kinds,
        ["line loading 10%", "replace loading 100%", "replace done"]
    );

    let mut later = Log::open(&s).await;
    let seen = later.until(|m| m["text"] == "done").await;
    assert_eq!(texts(&seen), ["done"]);
}

#[tokio::test]
async fn colour_codes_are_removed() {
    let s = setup("output-colour").await;
    strata_prints(&s, &["\u{1b}[32mINFO\u{1b}[0m:     ready\u{7}"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "INFO:     ready").await;
    let file = std::fs::read_to_string(s.dir.join("strata.log")).unwrap();
    assert_eq!(file, "INFO:     ready\n");
}

#[tokio::test]
async fn at_most_500_lines_are_kept() {
    let s = setup("output-limit").await;
    let lines: Vec<String> = (1..=520).map(|i| format!("line {i}")).collect();
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines,
            hang: true,
            ..Default::default()
        },
    );
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "line 520").await;
    let mut later = Log::open(&s).await;
    let seen = later.until(|m| m["text"] == "line 520").await;
    let kept = texts(&seen);
    assert_eq!(kept.len(), 500);
    assert_eq!(kept[0], "line 21");
}

#[tokio::test]
async fn a_restart_keeps_the_lines_before_it() {
    let s = setup("output-restart").await;
    strata_prints(&s, &["before the crash"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "before the crash").await;
    strata_prints(&s, &["after the restart"]);
    restart(&s).await;
    log.until(|m| m["text"] == "after the restart").await;

    let mut later = Log::open(&s).await;
    let seen = later.until(|m| m["text"] == "after the restart").await;
    let order: Vec<_> = seen
        .iter()
        .filter(|m| m["kind"] != "notice")
        .map(|m| m["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(order, ["started", "line", "started", "line"]);
    assert!(
        seen.iter()
            .all(|m| m["kind"] != "started" || m["at"].as_u64().is_some())
    );
}

/// Waits until the notice in effect is `reason`. It may already be: a
/// notice that does not change is not sent again, e.g. "not running" seen
/// once between gaming off and the start, then again after the end.
async fn notice(log: &mut Log, reason: &str) {
    if log.notice.as_deref() == Some(reason) {
        return;
    }
    log.until(|m| m["kind"] == "notice" && m["reason"] == reason)
        .await;
}

#[tokio::test]
async fn gaming_mode_is_a_notice() {
    let s = setup("output-gaming").await;
    strata_prints(&s, &["running"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    notice(&mut log, "running").await;
    post(&s, "/gaming", serde_json::json!({"enabled": true})).await;
    notice(&mut log, "gaming").await;
}

#[tokio::test]
async fn a_strata_started_by_hand_is_a_notice() {
    // Strata answers, but the service never started it: no output to show.
    let s = setup("output-by-hand").await;
    let mut log = Log::open(&s).await;
    notice(&mut log, "not_started_by_agent").await;
    assert_eq!(s.fake.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_strata_that_ended_is_a_notice() {
    let s = setup_with("output-ended", |c| c.auto_start = false).await;
    // Prints its lines, then ends.
    s.fake.command(
        "START-HERE.bat",
        FakeCommand {
            lines: vec!["Error while loading".into()],
            ..Default::default()
        },
    );
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "Error while loading").await;
    s.fake
        .strata_up
        .store(false, std::sync::atomic::Ordering::SeqCst);
    notice(&mut log, "not_running").await;
}

#[tokio::test]
async fn the_strata_key_never_shows() {
    let s = setup_with("output-key", |c| {
        c.strata_api_key = Some("sk-secret-123".into())
    })
    .await;
    strata_prints(&s, &["set STRATA_API_KEY=sk-secret-123"]);
    let mut log = Log::open(&s).await;
    restart(&s).await;
    log.until(|m| m["text"] == "set STRATA_API_KEY=***").await;
    let file = std::fs::read_to_string(s.dir.join("strata.log")).unwrap();
    assert!(!file.contains("sk-secret-123"), "{file}");
}
