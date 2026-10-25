//! The request log: each request through the proxy that made the model work,
//! with its key, model, type, tokens and speeds, live on the web UI's
//! *Requests* tab and kept over a restart. Never the text of a request.

use crate::common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use strata_nas_proxy::api_keys::ModelName;

const STREAM: &str = "/admin/api/requests-stream";
const CHAT: &str = "/v1/chat/completions";

/// A chunk as Strata's `openai_chunks` writes it (`serve/server.py`).
fn chunk(delta: &str, finish: &str, extra: &str) -> String {
    format!(
        r#"data: {{"id": "chatcmpl-1f0c", "object": "chat.completion.chunk", "created": 1761426000, "model": "Qwen3.6-27B", "choices": [{{"index": 0, "delta": {delta}, "finish_reason": {finish}}}]{extra}}}"#
    ) + "\n\n"
}

/// The last chunk's `usage` and llama.cpp's `timings` (`request_timings`).
const USAGE: &str = r#", "usage": {"prompt_tokens": 5200, "completion_tokens": 41, "total_tokens": 5241, "prompt_tokens_details": {"cached_tokens": 4800}}, "timings": {"cache_n": 4800, "prompt_n": 400, "prompt_ms": 250.0, "prompt_per_token_ms": 0.625, "prompt_per_second": 1600.0, "predicted_n": 41, "predicted_ms": 1025.0, "predicted_per_token_ms": 25.0, "predicted_per_second": 40.0}"#;

fn tool_call_stream() -> Vec<String> {
    vec![
        chunk(r#"{"role": "assistant", "content": ""}"#, "null", ""),
        chunk(
            r#"{"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "read", "arguments": ""}}]}"#,
            "null",
            "",
        ),
        chunk(
            r#"{"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\": \"a\"}"}}]}"#,
            "null",
            "",
        ),
        chunk("{}", r#""tool_calls""#, USAGE),
        "data: [DONE]\n\n".into(),
    ]
}

fn chat(stream: bool) -> Value {
    json!({"model": "any", "stream": stream,
           "messages": [{"role": "system", "content": "secret system"},
                        {"role": "user", "content": "secret question"}]})
}

async fn post_with(s: &Setup, key: &str, body: Value) -> reqwest::Response {
    post_json_to(&s.url, key, CHAT, body).await
}

/// The next row the stream sends after its first message.
async fn next_row(live: &mut Live) -> Value {
    live.until("row", |m| m["kind"] == "row").await["row"].clone()
}

/// The rows the stream sends first.
async fn all_rows(b: &Browser) -> Vec<Value> {
    let mut live = Live::open_at(b, STREAM).await;
    let all = live.until("all", |m| m["kind"] == "all").await;
    all["rows"].as_array().unwrap().clone()
}

#[tokio::test]
async fn a_streamed_chat_request_makes_a_row_with_tokens_speeds_and_type() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_stream(CHAT, tool_call_stream(), Duration::from_millis(50));
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let resp = post_json(&s, CHAT, chat(true)).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("call_1"), "{text}");

    let row = next_row(&mut live).await;
    assert_eq!(row["key"], "Test", "{row}");
    assert_eq!(row["model"], "Qwen3.6-27B", "{row}");
    assert_eq!(row["api"], "chat", "{row}");
    assert_eq!(row["start"], "prompt", "{row}");
    assert_eq!(row["answer"], "tool_call", "{row}");
    assert_eq!(row["prompt_tokens"], 5200, "{row}");
    assert_eq!(row["cached_tokens"], 4800, "{row}");
    assert_eq!(row["output_tokens"], 41, "{row}");
    assert_eq!(
        (&row["prefill_tokens"], &row["prefill_ms"]),
        (&json!(400), &json!(250.0))
    );
    assert_eq!(
        (&row["generation_tokens"], &row["generation_ms"]),
        (&json!(41), &json!(1025.0))
    );
    assert!(row["first_token_ms"].is_u64(), "{row}");
    assert_eq!(row["outcome"]["kind"], "done", "{row}");
    assert!(!row["time"].as_str().unwrap().is_empty(), "{row}");
    // Numbers and labels only.
    assert!(!row.to_string().contains("secret"), "{row}");
}

#[tokio::test]
async fn a_whole_chat_answer_makes_a_row_from_its_usage_and_timings() {
    let s = setup(true, |_| {}).await;
    let answer: Value = serde_json::from_str(&format!(
        r#"{{"id": "chatcmpl-1f0c", "object": "chat.completion", "created": 1761426000, "model": "Qwen3.6-27B", "choices": [{{"index": 0, "message": {{"role": "assistant", "content": "Hello"}}, "finish_reason": "stop"}}]{USAGE}}}"#
    ))
    .unwrap();
    s.pc.set_strata_json(CHAT, answer);
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    let row = next_row(&mut live).await;
    assert_eq!(row["answer"], "text", "{row}");
    assert_eq!(row["output_tokens"], 41, "{row}");
    assert_eq!(row["generation_ms"], 1025.0, "{row}");
    assert_eq!(row["prefill_tokens"], 400, "{row}");
    assert_eq!(row["first_token_ms"], Value::Null, "{row}");
}

#[tokio::test]
async fn a_key_that_hides_the_model_is_logged_with_the_real_model() {
    let s = setup(true, |c| c.hide_model = true).await;
    let (key, _) = s.state.api_keys.create("Hidden", ModelName::Hide).unwrap();
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let answer: Value = post_with(&s, &key, chat(false)).await.json().await.unwrap();
    assert_eq!(answer["model"], "strata", "{answer}");
    let row = next_row(&mut live).await;
    assert_eq!(row["key"], "Hidden", "{row}");
    assert_eq!(row["model"], "Qwen3-30B-A3B", "{row}");
}

#[tokio::test]
async fn requests_without_a_valid_key_and_the_model_list_make_no_row() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    assert_eq!(post_with(&s, "wrong", chat(false)).await.status(), 401);
    let list = reqwest::Client::new()
        .get(format!("{}/v1/models", s.url))
        .bearer_auth(&s.key)
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);

    let rows = eventually_rows(&b, 1).await;
    assert_eq!(rows[0]["key"], "Test", "{rows:?}");
}

/// The rows sent first, once there are `n` of them.
async fn eventually_rows(b: &Browser, n: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = all_rows(b).await;
        if rows.len() >= n || tokio::time::Instant::now() > deadline {
            assert_eq!(rows.len(), n, "{rows:?}");
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn only_the_newest_rows_are_kept() {
    let s = setup(true, |_| {}).await;
    s.state.requests.set_limit(2);
    let b = logged_in(&s.url).await;
    for _ in 0..3 {
        assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    }
    let rows = eventually_rows(&b, 2).await;
    let ids: Vec<u64> = rows.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    // Newest first, the first request gone.
    assert!(ids[0] > ids[1], "{ids:?}");
    let first_kept = ids[1];
    s.state.requests.set_limit(3);
    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    let rows = eventually_rows(&b, 3).await;
    assert_eq!(rows[2]["id"].as_u64(), Some(first_kept), "{rows:?}");
}

#[tokio::test]
async fn the_request_log_survives_a_restart() {
    let s = setup(true, |c| c.data_dir = Some(data_dir("request-log"))).await;
    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    let b = logged_in(&s.url).await;
    let before = eventually_rows(&b, 1).await;

    let url = serve_proxy(restarted(&s)).await;
    let b = logged_in_again(&url).await;
    let after = all_rows(&b).await;
    assert_eq!(after, before);
    // New requests go on from the last id.
    let s = Setup { url, ..s };
    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    let rows = eventually_rows(&b, 2).await;
    assert!(
        rows[0]["id"].as_u64() > before[0]["id"].as_u64(),
        "{rows:?}"
    );
}

#[tokio::test]
async fn a_client_that_hangs_up_mid_answer_cancelled_the_request() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_stream(CHAT, tool_call_stream(), Duration::from_secs(3));
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let resp = post_json(&s, CHAT, chat(true)).await;
    let mut body = resp.bytes_stream();
    body.next().await.unwrap().unwrap();
    drop(body);

    let row = next_row(&mut live).await;
    assert_eq!(row["outcome"]["kind"], "cancelled_by_client", "{row}");
    assert_eq!(row["output_tokens"], Value::Null, "{row}");
    assert_eq!(row["generation_ms"], Value::Null, "{row}");
    assert!(row["duration_ms"].is_u64(), "{row}");
}

#[tokio::test]
async fn the_request_log_needs_a_login() {
    let s = setup(true, |_| {}).await;
    let resp = Live::request_at(&Browser::new(&s.url), STREAM).await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn the_request_log_never_wakes_the_pc() {
    let s = setup(false, |_| {}).await;
    let b = logged_in(&s.url).await;
    let rows = all_rows(&b).await;
    assert!(rows.is_empty(), "{rows:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}
