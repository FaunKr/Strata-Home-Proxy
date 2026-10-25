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

/// Strata's metrics show nothing running: a request in flight is given up
/// after 300 ms.
async fn quiet_strata(awake: bool) -> Setup {
    let s = setup(awake, |_| {}).await;
    *s.pc.metrics.lock().unwrap() = Some(METRICS_IDLE.into());
    s.state.set_lost_after(Duration::from_millis(300));
    s.pc.set_strata_stream(CHAT, tool_call_stream(), Duration::from_secs(30));
    s
}

/// Sends a streamed chat request that is given up, and reads its row.
async fn lost_row(s: &Setup) -> Value {
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;
    let text = post_json(s, CHAT, chat(true)).await.text().await.unwrap();
    assert!(text.contains("no longer working"), "{text}");
    next_row(&mut live).await
}

#[tokio::test]
async fn a_streamed_request_given_up_is_lost() {
    let s = quiet_strata(true).await;
    let row = lost_row(&s).await;
    assert_eq!(row["outcome"]["kind"], "lost", "{row}");
    assert_eq!(row["output_tokens"], Value::Null, "{row}");
}

#[tokio::test]
async fn a_streamed_request_given_up_after_a_wake_is_lost() {
    let s = quiet_strata(false).await;
    let row = lost_row(&s).await;
    assert_eq!(row["outcome"]["kind"], "lost", "{row}");
    assert!(row["wait_ms"].as_u64().unwrap() > 0, "{row}");
}

#[tokio::test]
async fn a_strata_error_is_logged_with_its_status() {
    let s = setup(true, |_| {}).await;
    s.pc.chat_fails.store(true, Ordering::SeqCst);
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 500);
    let row = next_row(&mut live).await;
    assert_eq!(
        row["outcome"],
        json!({"kind": "strata_error", "status": 500}),
        "{row}"
    );
}

#[tokio::test]
async fn a_request_that_finishes_last_is_kept_over_a_newer_one() {
    let s = setup(true, |_| {}).await;
    s.state.requests.set_limit(1);
    s.pc.set_strata_stream(CHAT, tool_call_stream(), Duration::from_millis(1500));
    let b = logged_in(&s.url).await;
    let slow = {
        let s = s.clone();
        tokio::spawn(async move { post_json(&s, CHAT, chat(true)).await.text().await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(post_json(&s, CHAT, chat(false)).await.status(), 200);
    assert!(slow.await.unwrap().unwrap().contains("call_1"));

    let rows = eventually_rows(&b, 1).await;
    // The streamed one came first and finished last.
    assert_eq!(rows[0]["answer"], "tool_call", "{rows:?}");
}

const MESSAGES: &str = "/v1/messages";
const RESPONSES: &str = "/v1/responses";

/// An event as Strata's `anthropic_events` (`serve/server.py`) or its
/// Responses `Assembler` (`serve/responses.py`) writes it.
fn named_event(data: &str) -> String {
    let value: Value = serde_json::from_str(data).unwrap();
    format!(
        "event: {}\ndata: {data}\n\n",
        value["type"].as_str().unwrap()
    )
}

/// A messages answer that thinks, then calls a tool. The first piece ends
/// with the first token, the rest comes after the pause.
fn messages_stream() -> Vec<String> {
    vec![
        [
            r#"{"type": "message_start", "message": {"id": "msg_5c1f0e2a9b7d4c3e8f6a1b2c", "type": "message", "role": "assistant", "model": "Qwen3.6-27B", "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 5200, "output_tokens": 0}}}"#,
            r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}"#,
            r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Read a."}}"#,
        ]
        .map(named_event)
        .concat(),
        [
            r#"{"type": "content_block_stop", "index": 0}"#,
            r#"{"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}}"#,
            r#"{"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"a\"}"}}"#,
            r#"{"type": "content_block_stop", "index": 1}"#,
            r#"{"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null}, "usage": {"input_tokens": 1, "cache_creation_input_tokens": 399, "cache_read_input_tokens": 4800, "output_tokens": 41}}"#,
            r#"{"type": "message_stop"}"#,
        ]
        .map(named_event)
        .concat(),
    ]
}

/// A Responses answer that thinks, then writes; Strata counts its thinking.
fn responses_stream() -> Vec<String> {
    let response = |status: &str, usage: &str| {
        format!(
            r#"{{"id": "resp_9f2e4d6c8b0a1e3c5d7f9a2b", "object": "response", "created_at": 1761426000, "status": "{status}", "background": false, "error": null, "incomplete_details": null, "model": "Qwen3.6-27B", "output": [], "usage": {usage}}}"#
        )
    };
    let created = response("in_progress", "null");
    let done = response(
        "completed",
        r#"{"input_tokens": 5200, "input_tokens_details": {"cached_tokens": 4800}, "output_tokens": 41, "output_tokens_details": {"reasoning_tokens": 12}, "total_tokens": 5241}"#,
    );
    vec![
        [
            format!(r#"{{"type": "response.created", "sequence_number": 0, "response": {created}}}"#),
            r#"{"type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "status": "in_progress", "summary": [], "content": [{"type": "reasoning_text", "text": ""}]}}"#.into(),
            r#"{"type": "response.reasoning_text.delta", "sequence_number": 2, "item_id": "rs_1", "output_index": 0, "content_index": 0, "delta": "Hmm."}"#.into(),
        ]
        .map(|e| named_event(&e))
        .concat(),
        [
            r#"{"type": "response.output_item.added", "sequence_number": 4, "output_index": 1, "item": {"id": "msg_1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}"#.into(),
            r#"{"type": "response.output_text.delta", "sequence_number": 6, "item_id": "msg_1", "output_index": 1, "content_index": 0, "delta": "Hi", "logprobs": []}"#.into(),
            format!(r#"{{"type": "response.completed", "sequence_number": 9, "response": {done}}}"#),
        ]
        .map(|e| named_event(&e))
        .concat(),
    ]
}

#[tokio::test]
async fn a_streamed_messages_request_makes_a_row_with_measured_speeds_and_estimated_thinking() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_stream(MESSAGES, messages_stream(), Duration::from_millis(100));
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let body = json!({"model": "any", "stream": true, "max_tokens": 100,
                      "messages": [{"role": "user", "content": "secret question"}]});
    let resp = post_json(&s, MESSAGES, body).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("call_1"));

    let row = next_row(&mut live).await;
    assert_eq!(row["api"], "messages", "{row}");
    assert_eq!(row["model"], "Qwen3.6-27B", "{row}");
    assert_eq!(row["start"], "prompt", "{row}");
    assert_eq!(row["answer"], "tool_call", "{row}");
    assert_eq!(row["prompt_tokens"], 5200, "{row}");
    assert_eq!(row["cached_tokens"], 4800, "{row}");
    assert_eq!(row["output_tokens"], 41, "{row}");
    // 7 of 20 characters were thinking.
    assert_eq!(row["thinking_tokens"], 14, "{row}");
    assert_eq!(row["thinking_estimated"], true, "{row}");
    // Measured by the proxy: Strata sends no timings here.
    assert_eq!(row["prefill_tokens"], 400, "{row}");
    assert!(row["prefill_ms"].as_f64().unwrap() > 0.0, "{row}");
    assert_eq!(row["generation_tokens"], 40, "{row}");
    // The pause lies between the first and the last token; how much of it
    // the proxy sees depends on when the first piece reached it.
    assert!(row["generation_ms"].as_f64().unwrap() > 0.0, "{row}");
    assert!(row["first_token_ms"].is_u64(), "{row}");
    assert!(!row.to_string().contains("secret"), "{row}");
}

#[tokio::test]
async fn a_streamed_responses_request_makes_a_row_with_strata_s_thinking_count() {
    let s = setup(true, |_| {}).await;
    s.pc.set_strata_stream(RESPONSES, responses_stream(), Duration::from_millis(100));
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let body = json!({"model": "any", "stream": true, "input": "secret question"});
    let resp = post_json(&s, RESPONSES, body).await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();

    let row = next_row(&mut live).await;
    assert_eq!(row["api"], "responses", "{row}");
    assert_eq!(row["model"], "Qwen3.6-27B", "{row}");
    assert_eq!(row["start"], "prompt", "{row}");
    assert_eq!(row["answer"], "text", "{row}");
    assert_eq!(row["thinking_tokens"], 12, "{row}");
    assert_eq!(row.get("thinking_estimated"), None, "{row}");
    assert_eq!(row["prefill_tokens"], 400, "{row}");
    assert_eq!(row["generation_tokens"], 40, "{row}");
    // The pause lies between the first and the last token; how much of it
    // the proxy sees depends on when the first piece reached it.
    assert!(row["generation_ms"].as_f64().unwrap() > 0.0, "{row}");
}

#[tokio::test]
async fn an_images_request_makes_a_row_of_its_kind() {
    let s = setup(true, |_| {}).await;
    let b = logged_in(&s.url).await;
    let mut live = Live::open_at(&b, STREAM).await;
    live.until("all", |m| m["kind"] == "all").await;

    let body = json!({"model": "Qwen-Image-2.1-Turbo", "prompt": "secret cat"});
    let resp = post_json(&s, "/v1/images/generations", body).await;
    assert_eq!(resp.status(), 200);
    resp.bytes().await.unwrap();

    let row = next_row(&mut live).await;
    assert_eq!(row["api"], "images", "{row}");
    assert_eq!(row["model"], "Qwen-Image-2.1-Turbo", "{row}");
    assert_eq!(row["answer"], Value::Null, "{row}");
    assert_eq!(row["outcome"]["kind"], "done", "{row}");
}
