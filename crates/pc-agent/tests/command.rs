//! Running a command: output line by line, exit code and cancel.
#![cfg(unix)]

use std::time::Duration;

use strata_pc_agent::command::{CommandSpec, spawn};

fn sh(script: &str) -> CommandSpec {
    CommandSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        dir: None,
    }
}

#[tokio::test]
async fn output_arrives_line_by_line_with_the_exit_code() {
    // A progress bar rewrites its line with \r; each state counts as a line.
    let mut run = spawn(&sh(
        "echo eins; printf 'zwei 10%%\\rzwei 50%%\\n'; echo drei >&2; exit 3",
    ))
    .unwrap();
    let mut lines = Vec::new();
    while let Some(line) = run.next_line().await {
        lines.push(line);
    }
    lines.sort();
    assert_eq!(lines, ["drei", "eins", "zwei 10%", "zwei 50%"]);
    assert_eq!(run.wait().await, Some(3));
}

#[tokio::test]
async fn cancel_ends_the_command_and_what_it_started() {
    let mut run = spawn(&sh("echo los; sleep 30 & sleep 30; echo nie")).unwrap();
    assert_eq!(run.next_line().await.as_deref(), Some("los"));
    run.canceller()();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while run.next_line().await.is_some() {}
        run.wait().await
    })
    .await
    .expect("Befehl läuft nach dem Abbrechen weiter");
    assert_eq!(ended, None);
}

#[tokio::test]
async fn a_rewritten_line_keeps_its_carriage_return() {
    // Windows ends lines with \r\n; only a lone \r rewrites a line.
    let mut run = spawn(&sh("printf 'eins\\r\\nlädt 5%%\\rlädt 9%%\\r\\n'")).unwrap();
    let mut lines = Vec::new();
    while let Some(line) = run.next_output().await {
        lines.push(line);
    }
    assert_eq!(lines, ["eins", "lädt 5%\r", "lädt 9%"]);
}
