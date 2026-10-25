//! The request meter: what the request log learns about one request from
//! its body and from Strata's answer as it passes through. It reads only
//! roles, counts and times, never keeps text, and never changes a byte.

use std::time::Duration;

use serde_json::Value;
use strata_admin_ui::api::{RequestAnswer, RequestApi, RequestOutcome, RequestRow, RequestStart};

/// One request's measurements, fed while its answer passes through.
#[derive(Debug, Default)]
pub struct Meter {
    api: RequestApi,
    start: Option<RequestStart>,
    /// The bytes of an event that has not ended yet.
    pending: Vec<u8>,
    model: Option<String>,
    tool_call: bool,
    cut_off: bool,
    prompt_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    output_tokens: Option<u64>,
    prefill: Option<(u64, f64)>,
    generation: Option<(u64, f64)>,
    /// Since the request went to Strata.
    first_token: Option<Duration>,
}

impl Meter {
    /// A meter for a request to `path`, or `None` when requests there make
    /// no row in the request log.
    pub fn for_request(path: &str, body: &[u8]) -> Option<Self> {
        let path = path.split('?').next().unwrap_or_default();
        let path = path.strip_prefix("/v1").unwrap_or(path);
        let api = match path {
            "/chat/completions" => RequestApi::Chat,
            _ => return None,
        };
        let body: Option<Value> = serde_json::from_slice(body).ok();
        let start = body.as_ref().and_then(chat_start);
        Some(Self {
            api,
            start,
            ..Self::default()
        })
    }

    /// A piece of an event stream, `at` after the request went to Strata.
    /// Events may be split over pieces in any way.
    pub fn chunk(&mut self, bytes: &[u8], at: Duration) {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = event_end(&self.pending) {
            let event: Vec<u8> = self.pending.drain(..end).collect();
            for line in event.split(|&b| b == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                // `read` takes every event; only the first token's time stays.
                if let Some(data) = line.strip_prefix(b"data:")
                    && let Ok(value) = serde_json::from_slice::<Value>(data.trim_ascii())
                    && self.read(&value, &value["choices"][0]["delta"])
                    && self.first_token.is_none()
                {
                    self.first_token = Some(at);
                }
            }
        }
    }

    /// A whole JSON answer.
    pub fn whole(&mut self, bytes: &[u8]) {
        if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
            self.read(&value, &value["choices"][0]["message"]);
        }
    }

    /// Takes what one chunk or answer says; true if `part` carries content
    /// of the answer (thinking, text or a tool call).
    fn read(&mut self, value: &Value, part: &Value) -> bool {
        if self.model.is_none()
            && let Some(model) = value["model"].as_str()
        {
            self.model = Some(model.to_string());
        }
        let calls = part["tool_calls"].as_array().is_some_and(|c| !c.is_empty());
        match value["choices"][0]["finish_reason"].as_str() {
            Some("tool_calls") => self.tool_call = true,
            Some("length") => self.cut_off = true,
            _ => {}
        }
        self.tool_call |= calls;
        let usage = &value["usage"];
        if usage.is_object() {
            self.prompt_tokens = usage["prompt_tokens"].as_u64();
            self.cached_tokens = usage["prompt_tokens_details"]["cached_tokens"].as_u64();
            self.output_tokens = usage["completion_tokens"].as_u64();
        }
        let timings = &value["timings"];
        if timings.is_object() {
            self.prefill = pair(timings, "prompt_n", "prompt_ms");
            self.generation = pair(timings, "predicted_n", "predicted_ms");
        }
        let text = |key: &str| part[key].as_str().is_some_and(|t| !t.is_empty());
        calls || text("content") || text("reasoning_content")
    }

    /// The row's fields the meter knows, the others left as they are.
    pub fn row(&self) -> RequestRow {
        let answer = match (
            self.tool_call,
            self.output_tokens.is_some() || self.first_token.is_some(),
        ) {
            (true, _) => Some(RequestAnswer::ToolCall),
            (false, true) => Some(RequestAnswer::Text),
            (false, false) => None,
        };
        RequestRow {
            model: self.model.clone(),
            api: self.api,
            start: self.start,
            answer,
            prompt_tokens: self.prompt_tokens,
            cached_tokens: self.cached_tokens,
            output_tokens: self.output_tokens,
            prefill_tokens: self.prefill.map(|p| p.0),
            prefill_ms: self.prefill.map(|p| p.1),
            generation_tokens: self.generation.map(|g| g.0),
            generation_ms: self.generation.map(|g| g.1),
            first_token_ms: self.first_token.map(|t| t.as_millis() as u64),
            outcome: if self.cut_off {
                RequestOutcome::CutOff
            } else {
                RequestOutcome::Done
            },
            ..RequestRow::default()
        }
    }
}

/// Where the first event of `bytes` ends, after its blank line.
fn event_end(bytes: &[u8]) -> Option<usize> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Tokens and milliseconds from `timings`; none for zero tokens or time.
fn pair(timings: &Value, tokens: &str, ms: &str) -> Option<(u64, f64)> {
    let tokens = timings[tokens].as_u64()?;
    let ms = timings[ms].as_f64()?;
    (tokens > 0 && ms > 0.0).then_some((tokens, ms))
}

/// The role of the last entry of `messages`.
fn chat_start(body: &Value) -> Option<RequestStart> {
    match body["messages"].as_array()?.last()?["role"].as_str()? {
        "user" => Some(RequestStart::Prompt),
        "tool" => Some(RequestStart::ToolResult),
        "assistant" => Some(RequestStart::Continue),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_request_starts_with_its_last_messages_role() {
        let started = |role: &str| {
            let body = format!(
                r#"{{"messages":[{{"role":"system","content":"x"}},{{"role":"{role}","content":"y"}}]}}"#
            );
            Meter::for_request("/v1/chat/completions", body.as_bytes())
                .unwrap()
                .row()
                .start
        };
        assert_eq!(started("user"), Some(RequestStart::Prompt));
        assert_eq!(started("tool"), Some(RequestStart::ToolResult));
        assert_eq!(started("assistant"), Some(RequestStart::Continue));
    }

    /// A chunk as Strata's `openai_chunks` writes it (`serve/server.py`).
    fn chunk(delta: &str, finish: &str, extra: &str) -> String {
        format!(
            r#"data: {{"id": "chatcmpl-1f0c", "object": "chat.completion.chunk", "created": 1761426000, "model": "Qwen3.6-27B", "choices": [{{"index": 0, "delta": {delta}, "finish_reason": {finish}}}]{extra}}}"#
        ) + "\n\n"
    }

    /// The last chunk: `usage` and llama.cpp's `timings` (`request_timings`).
    const USAGE: &str = r#", "usage": {"prompt_tokens": 5200, "completion_tokens": 41, "total_tokens": 5241, "prompt_tokens_details": {"cached_tokens": 4800}}, "timings": {"cache_n": 4800, "prompt_n": 400, "prompt_ms": 250.0, "prompt_per_token_ms": 0.625, "prompt_per_second": 1600.0, "predicted_n": 41, "predicted_ms": 1025.0, "predicted_per_token_ms": 25.0, "predicted_per_second": 40.0}"#;

    fn chat_meter() -> Meter {
        Meter::for_request(
            "/v1/chat/completions",
            br#"{"stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_streamed_chat_answer_gives_tokens_speeds_and_tool_call() {
        let mut meter = chat_meter();
        let events = [
            chunk(r#"{"role": "assistant", "content": ""}"#, "null", ""),
            ": ping\n\n".to_string(),
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
            "data: [DONE]\n\n".to_string(),
        ];
        meter.chunk(events[0].as_bytes(), ms(5));
        meter.chunk(events[1].as_bytes(), ms(100));
        meter.chunk(events[2].as_bytes(), ms(300));
        // An event split over two chunks.
        let rest = events[3..].concat();
        let (a, b) = rest.split_at(40);
        meter.chunk(a.as_bytes(), ms(310));
        meter.chunk(b.as_bytes(), ms(1300));
        let row = meter.row();
        assert_eq!(row.model.as_deref(), Some("Qwen3.6-27B"));
        assert_eq!(row.start, Some(RequestStart::Prompt));
        assert_eq!(row.answer, Some(RequestAnswer::ToolCall));
        assert_eq!(row.prompt_tokens, Some(5200));
        assert_eq!(row.cached_tokens, Some(4800));
        assert_eq!(row.output_tokens, Some(41));
        assert_eq!(
            (row.prefill_tokens, row.prefill_ms),
            (Some(400), Some(250.0))
        );
        assert_eq!(
            (row.generation_tokens, row.generation_ms),
            (Some(41), Some(1025.0))
        );
        // The role-only chunk and the ping are no token.
        assert_eq!(row.first_token_ms, Some(300));
        assert_eq!(row.outcome, RequestOutcome::Done);
    }

    #[test]
    fn a_whole_chat_answer_gives_tokens_and_speeds_but_no_first_token() {
        let mut meter = chat_meter();
        // As `openai_collect` builds it from the chunks.
        let answer = format!(
            r#"{{"id": "chatcmpl-1f0c", "object": "chat.completion", "created": 1761426000, "model": "Qwen3.6-27B", "choices": [{{"index": 0, "message": {{"role": "assistant", "content": "Hello"}}, "finish_reason": "stop"}}]{USAGE}}}"#
        );
        meter.whole(answer.as_bytes());
        let row = meter.row();
        assert_eq!(row.model.as_deref(), Some("Qwen3.6-27B"));
        assert_eq!(row.answer, Some(RequestAnswer::Text));
        assert_eq!(row.output_tokens, Some(41));
        assert_eq!(row.generation_tok_s(), Some(40.0));
        assert_eq!(row.prefill_tok_s(), Some(1600.0));
        assert_eq!(row.first_token_ms, None);
    }

    #[test]
    fn a_chat_answer_stopped_at_its_limit_is_cut_off() {
        let mut meter = chat_meter();
        meter.chunk(
            chunk(r#"{"content": "Hel"}"#, "null", "").as_bytes(),
            ms(20),
        );
        meter.chunk(chunk("{}", r#""length""#, USAGE).as_bytes(), ms(40));
        let row = meter.row();
        assert_eq!(row.outcome, RequestOutcome::CutOff);
        assert_eq!(row.answer, Some(RequestAnswer::Text));
    }

    #[test]
    fn events_with_crlf_line_ends_are_read() {
        let mut meter = chat_meter();
        let event = chunk(r#"{"reasoning_content": "Hm"}"#, "null", "").replace('\n', "\r\n");
        meter.chunk(event.as_bytes(), ms(70));
        assert_eq!(meter.row().first_token_ms, Some(70));
    }

    #[test]
    fn a_fully_cached_prompt_has_no_prefill_speed() {
        let mut meter = chat_meter();
        let usage = USAGE.replace(r#""prompt_n": 400"#, r#""prompt_n": 0"#);
        meter.chunk(chunk("{}", r#""stop""#, &usage).as_bytes(), ms(40));
        let row = meter.row();
        assert_eq!(row.prefill_tok_s(), None);
        assert_eq!(row.generation_tok_s(), Some(40.0));
    }

    #[test]
    fn unreadable_bodies_and_answers_leave_fields_empty() {
        let mut meter = Meter::for_request("/v1/chat/completions", b"not json").unwrap();
        meter.chunk(b"data: {broken\n\n", ms(10));
        meter.whole(b"<html>");
        let row = meter.row();
        assert_eq!(row.start, None);
        assert_eq!(row.model, None);
        assert_eq!(row.answer, None);
        assert_eq!(row.output_tokens, None);
        assert_eq!(row.first_token_ms, None);
    }

    #[test]
    fn requests_that_make_no_row() {
        for path in ["/v1/models", "/models", "/health", "/v1/chat"] {
            assert!(Meter::for_request(path, b"{}").is_none(), "{path}");
        }
        assert!(Meter::for_request("/chat/completions?x=1", b"{}").is_some());
    }
}
