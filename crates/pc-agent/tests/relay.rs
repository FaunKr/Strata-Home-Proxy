//! Relay mode: Strata listens on the PC only, and the service passes the
//! proxy's requests on to it under `/relay`.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use strata_pc_agent::{managed::Managed, terminal::Terminal};
use strata_pc_protocol::RELAY_NO_STRATA;

/// The Strata Terminal with one model, served by the service; gives the
/// run config's path.
fn terminal_installed(s: &Setup) -> std::path::PathBuf {
    let cfg = &s.agent.cfg;
    let leaves = SetupLeaves::terminal(&cfg.strata_home, &cfg.strata_data_dir, "0.2.2");
    leaves
        .config(
            "iq3_xxs",
            json!({"exe": "component:strata-engine", "args": ["--native", "a.gguf"],
                "port": 8080, "host": "0.0.0.0", "api_key": "key-0123456789abcdef"}),
        )
        .download("IQ3_XXS", "a.gguf", "0123456789");
    let terminal = Terminal::of(cfg);
    let config = leaves.installation().config_path("iq3_xxs");
    s.agent
        .set_managed(Managed {
            strata_start: Some(terminal.program()),
            strata_args: Terminal::serve_args(&config, "0.0.0.0", "8080"),
            strata_processes: Some(terminal.processes()),
        })
        .unwrap();
    config
}

/// The Strata key in the run config, as the last start wrote it.
fn key(config: &std::path::Path) -> String {
    let fields: Value = serde_json::from_str(&std::fs::read_to_string(config).unwrap()).unwrap();
    fields["api_key"].as_str().unwrap().to_string()
}

fn relay_on(cfg: &mut Config) {
    cfg.relay = true;
}

#[tokio::test]
async fn the_relay_passes_the_request_on_with_the_strata_key() {
    let s = setup_with("relay-pass", relay_on).await;
    let config = terminal_installed(&s);
    s.agent.strata().start();
    let key = key(&config);

    let resp = client()
        .post(format!("{}/relay/v1/echo?stream=true", s.url))
        .bearer_auth("secret")
        .header("x-custom", "kept")
        .body("{\"model\": \"x\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let seen: Value = resp.json().await.unwrap();
    assert_eq!(
        seen,
        json!({
            "method": "POST",
            "path": "/v1/echo",
            "query": "stream=true",
            "authorization": format!("Bearer {key}"),
            "x_custom": "kept",
            "body": "{\"model\": \"x\"}",
        })
    );
}

#[tokio::test]
async fn the_relay_wants_the_token() {
    let s = setup_with("relay-token", relay_on).await;
    let resp = client()
        .get(format!("{}/relay/v1/echo", s.url))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn strata_s_own_errors_pass_unchanged() {
    let s = setup_with("relay-error", relay_on).await;
    let resp = client()
        .get(format!("{}/relay/v1/fail", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    assert!(resp.headers().get(RELAY_NO_STRATA).is_none());
    assert_eq!(resp.text().await.unwrap(), "engine says no");
}

#[tokio::test]
async fn without_strata_the_relay_answers_a_marked_502() {
    let s = setup_with("relay-down", |cfg| {
        cfg.relay = true;
        // Nothing listens there.
        cfg.strata_url = "http://127.0.0.1:1".into();
    })
    .await;
    let resp = client()
        .get(format!("{}/relay/health", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    assert!(resp.headers().get(RELAY_NO_STRATA).is_some());
}

#[tokio::test]
async fn the_relay_streams_and_lets_go_when_the_caller_leaves() {
    let s = setup_with("relay-stream", relay_on).await;
    let resp = client()
        .get(format!("{}/relay/v1/stream", s.url))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut body = resp.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("the first event arrives before the answer ends")
        .unwrap()
        .unwrap();
    assert_eq!(&first[..], b"data: first\n\n");
    assert!(!s.fake.stream_dropped.load(Ordering::SeqCst));
    drop(body);
    eventually("Strata's request dropped", || {
        s.fake.stream_dropped.load(Ordering::SeqCst)
    })
    .await;
}

#[tokio::test]
async fn in_relay_mode_the_terminal_s_strata_listens_on_the_pc_only() {
    let s = setup_with("relay-host", relay_on).await;
    let config = terminal_installed(&s);
    s.agent.strata().start();
    let (_, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(args, Terminal::serve_args(&config, "127.0.0.1", "8080"));
    assert_eq!(get(&s, "/status").await["relay"], json!({"on": true}));
}

#[tokio::test]
async fn without_relay_mode_strata_listens_for_the_nas() {
    let s = setup("relay-off-host").await;
    let config = terminal_installed(&s);
    s.agent.strata().start();
    let (_, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(args, Terminal::serve_args(&config, "0.0.0.0", "8080"));
    assert_eq!(get(&s, "/status").await["relay"], json!({"on": false}));
}

#[tokio::test]
async fn an_old_strata_folder_s_start_is_left_as_it_is() {
    let s = setup_with("relay-source", relay_on).await;
    s.agent.strata().start();
    let (program, args) = s.fake.started.lock().unwrap().clone().unwrap();
    assert_eq!(program, std::path::PathBuf::from("START-HERE.bat"));
    assert!(args.is_empty(), "{args:?}");
    assert_eq!(
        get(&s, "/status").await["relay"],
        json!({"on": false}),
        "the proxy keeps the direct path"
    );
}

#[tokio::test]
async fn a_restarted_service_reports_the_mode_of_the_strata_it_finds() {
    let s = setup_with("relay-restarted", relay_on).await;
    terminal_installed(&s);
    let agent = s.restarted();
    assert!(agent.status().await.relay.unwrap().on);
}
