//! The request meter: what the request log learns about one request from
//! its body and from Strata's answer as it passes through. It reads only
//! roles, counts and times, never keeps text, and never changes a byte.
//! What it reads per API: docs/strata-api.md, "What the proxy reads from
//! forwarded answers".

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
    /// Strata's own count, on the Responses API only.
    thinking_tokens: Option<u64>,
    /// Characters of thinking, and of all the answer's content with it.
    thinking_chars: u64,
    content_chars: u64,
    prefill: Option<(u64, f64)>,
    generation: Option<(u64, f64)>,
    /// Since the request went to Strata.
    first_token: Option<Duration>,
    last_token: Option<Duration>,
}

/// The content one event or answer carries, counted in characters.
#[derive(Default)]
struct Content {
    thinking: u64,
    /// Text and tool call arguments.
    other: u64,
    /// A tool call begins: content even before its arguments.
    call: bool,
}

impl Content {
    fn of(thinking: &str, other: &str) -> Self {
        Self {
            thinking: thinking.chars().count() as u64,
            other: other.chars().count() as u64,
            call: false,
        }
    }

    fn call() -> Self {
        Self {
            call: true,
            ..Self::default()
        }
    }

    fn any(&self) -> bool {
        self.call || self.thinking + self.other > 0
    }

    fn add(&mut self, more: Self) {
        self.thinking += more.thinking;
        self.other += more.other;
        self.call |= more.call;
    }
}

impl Meter {
    /// A meter for a request to `path`, or `None` when requests there make
    /// no row in the request log.
    pub fn for_request(path: &str, body: &[u8]) -> Option<Self> {
        let path = path.split('?').next().unwrap_or_default();
        let path = path.strip_prefix("/v1").unwrap_or(path);
        let api = match path {
            "/chat/completions" => RequestApi::Chat,
            "/messages" => RequestApi::Messages,
            "/responses" => RequestApi::Responses,
            "/completions" => RequestApi::Completions,
            "/embeddings" => RequestApi::Embeddings,
            "/images/generations" => RequestApi::Images,
            _ => return None,
        };
        let body: Option<Value> = serde_json::from_slice(body).ok();
        let start = body.as_ref().and_then(|body| match api {
            RequestApi::Chat => chat_start(body),
            RequestApi::Messages => messages_start(body),
            RequestApi::Responses => responses_start(body),
            _ => None,
        });
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
                if let Some(data) = line.strip_prefix(b"data:")
                    && let Ok(value) = serde_json::from_slice::<Value>(data.trim_ascii())
                {
                    let content = self.event(&value);
                    if content.any() {
                        self.first_token.get_or_insert(at);
                        self.last_token = Some(at);
                    }
                    self.add_chars(content);
                }
            }
        }
    }

    /// A whole JSON answer.
    pub fn whole(&mut self, bytes: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return;
        };
        let content = match self.api {
            RequestApi::Chat => self.chat(&value, &value["choices"][0]["message"]),
            RequestApi::Messages => self.whole_message(&value),
            RequestApi::Responses => {
                self.response(&value);
                Content::default()
            }
            _ => {
                self.model_from(&value["model"]);
                Content::default()
            }
        };
        self.add_chars(content);
    }

    fn add_chars(&mut self, content: Content) {
        self.thinking_chars += content.thinking;
        self.content_chars += content.thinking + content.other;
    }

    /// Takes what one event of a stream says, and returns its content.
    fn event(&mut self, value: &Value) -> Content {
        match self.api {
            RequestApi::Chat => self.chat(value, &value["choices"][0]["delta"]),
            RequestApi::Messages => self.message_event(value),
            RequestApi::Responses => self.responses_event(value),
            _ => Content::default(),
        }
    }

    fn model_from(&mut self, model: &Value) {
        if self.model.is_none()
            && let Some(model) = model.as_str()
        {
            self.model = Some(model.to_string());
        }
    }

    /// A chat chunk (`part` its `delta`) or a whole chat answer (`part` its
    /// `message`).
    fn chat(&mut self, value: &Value, part: &Value) -> Content {
        self.model_from(&value["model"]);
        let calls = part["tool_calls"].as_array().filter(|c| !c.is_empty());
        match value["choices"][0]["finish_reason"].as_str() {
            Some("tool_calls") => self.tool_call = true,
            Some("length") => self.cut_off = true,
            _ => {}
        }
        self.tool_call |= calls.is_some();
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
        let text = |key: &str| part[key].as_str().unwrap_or_default();
        let mut content = Content::of(text("reasoning_content"), text("content"));
        if calls.is_some() {
            content.add(Content::call());
        }
        for call in calls.into_iter().flatten() {
            let arguments = &call["function"]["arguments"];
            content.add(Content::of("", arguments.as_str().unwrap_or_default()));
        }
        content
    }

    /// An event of a messages stream (`serve/server.py`, `anthropic_events`).
    fn message_event(&mut self, value: &Value) -> Content {
        match value["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.model_from(&value["message"]["model"]);
                self.message_usage(&value["message"]["usage"]);
            }
            "content_block_start" if value["content_block"]["type"] == "tool_use" => {
                self.tool_call = true;
                return Content::call();
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                let text = |key: &str| delta[key].as_str().unwrap_or_default();
                return match delta["type"].as_str().unwrap_or_default() {
                    "thinking_delta" => Content::of(text("thinking"), ""),
                    "text_delta" => Content::of("", text("text")),
                    "input_json_delta" => Content::of("", text("partial_json")),
                    _ => Content::default(),
                };
            }
            "message_delta" => {
                self.stop_reason(&value["delta"]["stop_reason"]);
                self.message_usage(&value["usage"]);
            }
            _ => {}
        }
        Content::default()
    }

    /// A whole messages answer (`anthropic_collect`).
    fn whole_message(&mut self, value: &Value) -> Content {
        self.model_from(&value["model"]);
        self.stop_reason(&value["stop_reason"]);
        self.message_usage(&value["usage"]);
        let mut content = Content::default();
        for block in value["content"].as_array().into_iter().flatten() {
            let text = |key: &str| block[key].as_str().unwrap_or_default();
            match block["type"].as_str().unwrap_or_default() {
                "thinking" => content.add(Content::of(text("thinking"), "")),
                "text" => content.add(Content::of("", text("text"))),
                "tool_use" => {
                    self.tool_call = true;
                    content.add(Content::of("", &block["input"].to_string()));
                }
                _ => {}
            }
        }
        content
    }

    fn stop_reason(&mut self, reason: &Value) {
        match reason.as_str() {
            Some("tool_use") => self.tool_call = true,
            Some("max_tokens") => self.cut_off = true,
            _ => {}
        }
    }

    /// Messages' `usage`: the prompt is what was read, written to the cache
    /// and read from it together. The last one seen wins: `message_delta`'s
    /// completes `message_start`'s.
    fn message_usage(&mut self, usage: &Value) {
        if !usage.is_object() {
            return;
        }
        let tokens = |key: &str| usage[key].as_u64();
        self.prompt_tokens = tokens("input_tokens").map(|input| {
            input
                + tokens("cache_creation_input_tokens").unwrap_or(0)
                + tokens("cache_read_input_tokens").unwrap_or(0)
        });
        self.cached_tokens = tokens("cache_read_input_tokens");
        self.output_tokens = tokens("output_tokens");
    }

    /// An event of a Responses stream (`serve/responses.py`, `Assembler`).
    fn responses_event(&mut self, value: &Value) -> Content {
        let delta = || value["delta"].as_str().unwrap_or_default();
        match value["type"].as_str().unwrap_or_default() {
            "response.reasoning_text.delta" => Content::of(delta(), ""),
            "response.output_text.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta" => Content::of("", delta()),
            "response.output_item.added" if is_call(&value["item"]) => {
                self.tool_call = true;
                Content::call()
            }
            _ => {
                // response.created, .in_progress, .completed, .incomplete and
                // .failed carry the whole response so far.
                if value["response"].is_object() {
                    self.response(&value["response"]);
                }
                // A Responses answer counts no characters: Strata gives the
                // thinking tokens itself.
                Content::default()
            }
        }
    }

    /// A Responses answer: whole, or as an event carries it so far.
    fn response(&mut self, response: &Value) {
        self.model_from(&response["model"]);
        let output = response["output"].as_array().into_iter().flatten();
        self.tool_call |= output.into_iter().any(is_call);
        self.cut_off |= response["status"] == "incomplete";
        let usage = &response["usage"];
        if usage.is_object() {
            self.prompt_tokens = usage["input_tokens"].as_u64();
            self.cached_tokens = usage["input_tokens_details"]["cached_tokens"].as_u64();
            self.output_tokens = usage["output_tokens"].as_u64();
            self.thinking_tokens = usage["output_tokens_details"]["reasoning_tokens"].as_u64();
        }
    }

    /// The row's fields the meter knows, the others left as they are.
    pub fn row(&self) -> RequestRow {
        let answer = match (
            self.tool_call,
            self.output_tokens.is_some() || self.first_token.is_some(),
        ) {
            _ if !self.api.has_turns() => None,
            (true, _) => Some(RequestAnswer::ToolCall),
            (false, true) => Some(RequestAnswer::Text),
            (false, false) => None,
        };
        let (thinking_tokens, thinking_estimated) = self.thinking();
        let prefill = self.prefill.or_else(|| self.measured_prefill());
        let generation = self.generation.or_else(|| self.measured_generation());
        RequestRow {
            model: self.model.clone(),
            api: self.api,
            start: self.start,
            answer,
            prompt_tokens: self.prompt_tokens,
            cached_tokens: self.cached_tokens,
            thinking_tokens,
            thinking_estimated,
            output_tokens: self.output_tokens,
            prefill_tokens: prefill.map(|p| p.0),
            prefill_ms: prefill.map(|p| p.1),
            generation_tokens: generation.map(|g| g.0),
            generation_ms: generation.map(|g| g.1),
            first_token_ms: self.first_token.map(|t| t.as_millis() as u64),
            outcome: if self.cut_off {
                RequestOutcome::CutOff
            } else {
                RequestOutcome::Done
            },
            ..RequestRow::default()
        }
    }

    /// Strata's count on Responses. Elsewhere the thinking text's share of
    /// the answer's characters, times the output tokens: an estimate.
    fn thinking(&self) -> (Option<u64>, bool) {
        if self.api == RequestApi::Responses {
            return (self.thinking_tokens, false);
        }
        let output = match self.output_tokens {
            Some(output) if self.content_chars > 0 => output,
            _ => return (None, false),
        };
        if self.thinking_chars == 0 {
            return (Some(0), false);
        }
        let share = self.thinking_chars as f64 / self.content_chars as f64;
        (Some((share * output as f64).round() as u64), true)
    }

    /// The prompt tokens Strata read over the time to the first token.
    fn measured_prefill(&self) -> Option<(u64, f64)> {
        let read = self
            .prompt_tokens?
            .saturating_sub(self.cached_tokens.unwrap_or(0));
        speed_sample(read, self.first_token?)
    }

    /// The tokens after the first over the time from the first to the last.
    fn measured_generation(&self) -> Option<(u64, f64)> {
        let first = self.first_token?;
        let span = self.last_token?.checked_sub(first)?;
        speed_sample(self.output_tokens?.checked_sub(1)?, span)
    }
}

/// Tokens and milliseconds for a speed; none for zero tokens or time.
fn speed_sample(tokens: u64, time: Duration) -> Option<(u64, f64)> {
    let ms = time.as_secs_f64() * 1000.0;
    (tokens > 0 && ms > 0.0).then_some((tokens, ms))
}

/// An output item that calls a tool.
fn is_call(item: &Value) -> bool {
    matches!(
        item["type"].as_str(),
        Some("function_call" | "custom_tool_call")
    )
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

/// The last message: a user's that holds a `tool_result` block is a tool's
/// result.
fn messages_start(body: &Value) -> Option<RequestStart> {
    let last = body["messages"].as_array()?.last()?;
    match last["role"].as_str()? {
        "user" => {
            let blocks = last["content"].as_array().into_iter().flatten();
            Some(if blocks.into_iter().any(|b| b["type"] == "tool_result") {
                RequestStart::ToolResult
            } else {
                RequestStart::Prompt
            })
        }
        "assistant" => Some(RequestStart::Continue),
        _ => None,
    }
}

/// `input`: a string is a prompt; of a list, the last item says.
fn responses_start(body: &Value) -> Option<RequestStart> {
    let input = &body["input"];
    if input.is_string() {
        return Some(RequestStart::Prompt);
    }
    let last = input.as_array()?.last()?;
    match (last["type"].as_str(), last["role"].as_str()) {
        (Some("function_call_output" | "custom_tool_call_output"), _) => {
            Some(RequestStart::ToolResult)
        }
        (None | Some("message"), Some("user")) => Some(RequestStart::Prompt),
        (None | Some("message"), Some("assistant")) => Some(RequestStart::Continue),
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
        for path in [
            "/v1/models",
            "/models",
            "/health",
            "/v1/chat",
            "/v1/messages/count_tokens",
            "/v1/responses/resp_1",
        ] {
            assert!(Meter::for_request(path, b"{}").is_none(), "{path}");
        }
        assert!(Meter::for_request("/chat/completions?x=1", b"{}").is_some());
    }

    /// A messages event as `anthropic_events` writes it (`serve/server.py`).
    fn message_event(name: &str, data: &str) -> String {
        format!("event: {name}\ndata: {data}\n\n")
    }

    const MESSAGE_START: &str = r#"{"type": "message_start", "message": {"id": "msg_5c1f0e2a9b7d4c3e8f6a1b2c", "type": "message", "role": "assistant", "model": "Qwen3.6-27B", "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 5200, "output_tokens": 0}}}"#;

    /// `message_delta` with the final counts: 400 read, 4800 from the cache.
    fn message_delta(stop: &str) -> String {
        message_event(
            "message_delta",
            &format!(
                r#"{{"type": "message_delta", "delta": {{"stop_reason": "{stop}", "stop_sequence": null}}, "usage": {{"input_tokens": 1, "cache_creation_input_tokens": 399, "cache_read_input_tokens": 4800, "output_tokens": 41}}}}"#
            ),
        )
    }

    fn messages_meter() -> Meter {
        Meter::for_request(
            "/v1/messages",
            br#"{"stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_messages_request_starts_with_its_last_message() {
        let started = |last: &str| {
            let body = format!(r#"{{"messages":[{{"role":"user","content":"x"}},{last}]}}"#);
            Meter::for_request("/v1/messages?beta=true", body.as_bytes())
                .unwrap()
                .row()
                .start
        };
        assert_eq!(
            started(r#"{"role":"user","content":"y"}"#),
            Some(RequestStart::Prompt)
        );
        assert_eq!(
            started(r#"{"role":"user","content":[{"type":"text","text":"y"}]}"#),
            Some(RequestStart::Prompt)
        );
        assert_eq!(
            started(
                r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"y"}]}"#
            ),
            Some(RequestStart::ToolResult)
        );
        assert_eq!(
            started(r#"{"role":"assistant","content":"y"}"#),
            Some(RequestStart::Continue)
        );
    }

    #[test]
    fn a_responses_request_starts_with_its_input() {
        let started = |input: &str| {
            let body = format!(r#"{{"input":{input}}}"#);
            Meter::for_request("/v1/responses", body.as_bytes())
                .unwrap()
                .row()
                .start
        };
        assert_eq!(started(r#""hi""#), Some(RequestStart::Prompt));
        assert_eq!(
            started(r#"[{"type":"message","role":"user","content":"hi"}]"#),
            Some(RequestStart::Prompt)
        );
        assert_eq!(
            started(r#"[{"role":"user","content":"hi"}]"#),
            Some(RequestStart::Prompt)
        );
        assert_eq!(
            started(
                r#"[{"role":"user","content":"hi"},{"type":"function_call","call_id":"c","name":"read","arguments":"{}"},{"type":"function_call_output","call_id":"c","output":"x"}]"#
            ),
            Some(RequestStart::ToolResult)
        );
        assert_eq!(
            started(r#"[{"type":"message","role":"assistant","content":"hi"}]"#),
            Some(RequestStart::Continue)
        );
        assert_eq!(started("[]"), None);
    }

    #[test]
    fn a_streamed_messages_answer_gives_tokens_measured_speeds_and_an_estimate() {
        let mut meter = messages_meter();
        meter.chunk(
            message_event("message_start", MESSAGE_START).as_bytes(),
            ms(5),
        );
        meter.chunk(b": keep-alive\n\n", ms(100));
        meter.chunk(
            message_event(
                "content_block_start",
                r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}"#,
            )
            .as_bytes(),
            ms(240),
        );
        meter.chunk(
            message_event(
                "content_block_delta",
                r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Read a."}}"#,
            )
            .as_bytes(),
            ms(250),
        );
        let rest = [
            message_event(
                "content_block_stop",
                r#"{"type": "content_block_stop", "index": 0}"#,
            ),
            message_event(
                "content_block_start",
                r#"{"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_1", "name": "read", "input": {}}}"#,
            ),
            message_event(
                "content_block_delta",
                r#"{"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"a\"}"}}"#,
            ),
        ]
        .concat();
        meter.chunk(rest.as_bytes(), ms(1250));
        let end = [
            message_event(
                "content_block_stop",
                r#"{"type": "content_block_stop", "index": 1}"#,
            ),
            message_delta("tool_use"),
            message_event("message_stop", r#"{"type": "message_stop"}"#),
        ]
        .concat();
        meter.chunk(end.as_bytes(), ms(1260));
        let row = meter.row();
        assert_eq!(row.api, RequestApi::Messages);
        assert_eq!(row.model.as_deref(), Some("Qwen3.6-27B"));
        assert_eq!(row.start, Some(RequestStart::Prompt));
        assert_eq!(row.answer, Some(RequestAnswer::ToolCall));
        assert_eq!(row.prompt_tokens, Some(5200));
        assert_eq!(row.cached_tokens, Some(4800));
        assert_eq!(row.output_tokens, Some(41));
        // 7 of 20 characters were thinking: 14.35 of 41 tokens.
        assert_eq!(
            (row.thinking_tokens, row.thinking_estimated),
            (Some(14), true)
        );
        // The block's start is no token; its first delta is.
        assert_eq!(row.first_token_ms, Some(250));
        // 400 uncached prompt tokens in 250 ms; 40 more tokens in 1000 ms.
        assert_eq!(row.prefill_tok_s(), Some(1600.0));
        assert_eq!(row.generation_tok_s(), Some(40.0));
        assert_eq!(row.outcome, RequestOutcome::Done);
    }

    #[test]
    fn a_messages_answer_stopped_at_its_limit_is_cut_off() {
        let mut meter = messages_meter();
        let events = [
            message_event("message_start", MESSAGE_START),
            message_event(
                "content_block_delta",
                r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}"#,
            ),
            message_delta("max_tokens"),
        ]
        .concat();
        meter.chunk(events.as_bytes(), ms(30));
        let row = meter.row();
        assert_eq!(row.outcome, RequestOutcome::CutOff);
        assert_eq!(row.answer, Some(RequestAnswer::Text));
        assert_eq!(
            (row.thinking_tokens, row.thinking_estimated),
            (Some(0), false)
        );
    }

    #[test]
    fn a_whole_messages_answer_gives_tokens_and_an_estimate_but_no_speeds() {
        let mut meter = messages_meter();
        // As `anthropic_collect` builds it from the events.
        meter.whole(br#"{"id": "msg_5c1f0e2a9b7d4c3e8f6a1b2c", "type": "message", "role": "assistant", "model": "Qwen3.6-27B", "content": [{"type": "thinking", "thinking": "Hmm, hm.", "signature": ""}, {"type": "text", "text": "Hi there"}], "stop_reason": "end_turn", "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 41, "cache_creation_input_tokens": 399, "cache_read_input_tokens": 4800}}"#);
        let row = meter.row();
        assert_eq!(row.model.as_deref(), Some("Qwen3.6-27B"));
        assert_eq!(row.answer, Some(RequestAnswer::Text));
        assert_eq!(row.prompt_tokens, Some(5200));
        assert_eq!(row.cached_tokens, Some(4800));
        // 8 of 16 characters: 20.5 rounds to 21.
        assert_eq!(
            (row.thinking_tokens, row.thinking_estimated),
            (Some(21), true)
        );
        assert_eq!(row.first_token_ms, None);
        assert_eq!(row.prefill_tok_s(), None);
        assert_eq!(row.generation_tok_s(), None);
    }

    #[test]
    fn a_chat_answer_with_reasoning_gives_an_estimate() {
        let mut meter = chat_meter();
        meter.chunk(
            chunk(r#"{"reasoning_content": "abc"}"#, "null", "").as_bytes(),
            ms(10),
        );
        meter.chunk(chunk(r#"{"content": "d"}"#, "null", "").as_bytes(), ms(20));
        meter.chunk(chunk("{}", r#""stop""#, USAGE).as_bytes(), ms(30));
        let row = meter.row();
        // 3 of 4 characters: 30.75 of 41 tokens.
        assert_eq!(
            (row.thinking_tokens, row.thinking_estimated),
            (Some(31), true)
        );
        // Strata's timings, not the proxy's clock.
        assert_eq!(row.generation_tok_s(), Some(40.0));
    }

    /// A Responses event as the `Assembler` writes it (`serve/responses.py`).
    fn response_event(data: &str) -> String {
        let value: Value = serde_json::from_str(data).unwrap();
        format!(
            "event: {}\ndata: {data}\n\n",
            value["type"].as_str().unwrap()
        )
    }

    fn response(status: &str, usage: &str) -> String {
        format!(
            r#"{{"id": "resp_9f2e4d6c8b0a1e3c5d7f9a2b", "object": "response", "created_at": 1761426000, "status": "{status}", "background": false, "error": null, "incomplete_details": null, "model": "Qwen3.6-27B", "output": [], "usage": {usage}}}"#
        )
    }

    const RESPONSE_USAGE: &str = r#"{"input_tokens": 5200, "input_tokens_details": {"cached_tokens": 4800}, "output_tokens": 41, "output_tokens_details": {"reasoning_tokens": 12}, "total_tokens": 5241}"#;

    fn responses_meter() -> Meter {
        Meter::for_request("/v1/responses", br#"{"stream":true,"input":"hi"}"#).unwrap()
    }

    #[test]
    fn a_streamed_responses_answer_gives_exact_thinking_and_measured_speeds() {
        let mut meter = responses_meter();
        let created = response("in_progress", "null");
        let start = [
            response_event(&format!(
                r#"{{"type": "response.created", "sequence_number": 0, "response": {created}}}"#
            )),
            response_event(&format!(
                r#"{{"type": "response.in_progress", "sequence_number": 1, "response": {created}}}"#
            )),
            response_event(
                r#"{"type": "response.output_item.added", "sequence_number": 2, "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "status": "in_progress", "summary": [], "content": [{"type": "reasoning_text", "text": ""}]}}"#,
            ),
        ]
        .concat();
        meter.chunk(start.as_bytes(), ms(5));
        meter.chunk(
            response_event(
                r#"{"type": "response.reasoning_text.delta", "sequence_number": 3, "item_id": "rs_1", "output_index": 0, "content_index": 0, "delta": "Read a."}"#,
            )
            .as_bytes(),
            ms(250),
        );
        let call = [
            response_event(
                r#"{"type": "response.reasoning_text.done", "sequence_number": 4, "item_id": "rs_1", "output_index": 0, "content_index": 0, "text": "Read a."}"#,
            ),
            response_event(
                r#"{"type": "response.output_item.added", "sequence_number": 6, "output_index": 1, "item": {"id": "fc_1", "type": "function_call", "status": "in_progress", "call_id": "call_1", "name": "read", "arguments": ""}}"#,
            ),
            response_event(
                r#"{"type": "response.function_call_arguments.delta", "sequence_number": 7, "item_id": "fc_1", "output_index": 1, "delta": "{\"path\": \"a\"}"}"#,
            ),
        ]
        .concat();
        meter.chunk(call.as_bytes(), ms(1250));
        let done = response("completed", RESPONSE_USAGE);
        meter.chunk(
            response_event(&format!(
                r#"{{"type": "response.completed", "sequence_number": 10, "response": {done}}}"#
            ))
            .as_bytes(),
            ms(1260),
        );
        let row = meter.row();
        assert_eq!(row.api, RequestApi::Responses);
        assert_eq!(row.model.as_deref(), Some("Qwen3.6-27B"));
        assert_eq!(row.start, Some(RequestStart::Prompt));
        assert_eq!(row.answer, Some(RequestAnswer::ToolCall));
        assert_eq!(row.prompt_tokens, Some(5200));
        assert_eq!(row.cached_tokens, Some(4800));
        assert_eq!(row.output_tokens, Some(41));
        assert_eq!(
            (row.thinking_tokens, row.thinking_estimated),
            (Some(12), false)
        );
        assert_eq!(row.first_token_ms, Some(250));
        assert_eq!(row.prefill_tok_s(), Some(1600.0));
        assert_eq!(row.generation_tok_s(), Some(40.0));
        assert_eq!(row.outcome, RequestOutcome::Done);
    }

    #[test]
    fn a_responses_answer_out_of_tokens_is_cut_off() {
        let mut meter = responses_meter();
        let incomplete = response("incomplete", RESPONSE_USAGE).replace(
            r#""incomplete_details": null"#,
            r#""incomplete_details": {"reason": "max_output_tokens"}"#,
        );
        meter.chunk(
            response_event(&format!(
                r#"{{"type": "response.incomplete", "sequence_number": 9, "response": {incomplete}}}"#
            ))
            .as_bytes(),
            ms(30),
        );
        let row = meter.row();
        assert_eq!(row.outcome, RequestOutcome::CutOff);
        assert_eq!(row.answer, Some(RequestAnswer::Text));
    }

    #[test]
    fn a_whole_responses_answer_gives_tokens_and_the_tool_call() {
        let mut meter = responses_meter();
        // As `collect` returns it: the last event's response.
        let answer = response("completed", RESPONSE_USAGE).replace(
            r#""output": []"#,
            r#""output": [{"id": "fc_1", "type": "function_call", "status": "completed", "call_id": "call_1", "name": "read", "arguments": "{}"}]"#,
        );
        meter.whole(answer.as_bytes());
        let row = meter.row();
        assert_eq!(row.answer, Some(RequestAnswer::ToolCall));
        assert_eq!(row.thinking_tokens, Some(12));
        assert_eq!(row.generation_tok_s(), None);
    }

    #[test]
    fn a_responses_answer_without_strata_s_count_gets_no_estimate() {
        let mut meter = responses_meter();
        meter.chunk(
            response_event(
                r#"{"type": "response.reasoning_text.delta", "sequence_number": 3, "item_id": "rs_1", "output_index": 0, "content_index": 0, "delta": "Read a."}"#,
            )
            .as_bytes(),
            ms(250),
        );
        let usage =
            RESPONSE_USAGE.replace(r#", "output_tokens_details": {"reasoning_tokens": 12}"#, "");
        let done = response("completed", &usage);
        meter.chunk(
            response_event(&format!(
                r#"{{"type": "response.completed", "sequence_number": 10, "response": {done}}}"#
            ))
            .as_bytes(),
            ms(1260),
        );
        let row = meter.row();
        assert_eq!(row.output_tokens, Some(41));
        assert_eq!((row.thinking_tokens, row.thinking_estimated), (None, false));
    }

    #[test]
    fn no_measured_speeds_for_one_token_or_a_zero_span() {
        let streamed = |output: u64, last: u64| {
            let mut meter = messages_meter();
            let delta = message_delta("end_turn").replace(
                r#""output_tokens": 41"#,
                &format!(r#""output_tokens": {output}"#),
            );
            let text = message_event(
                "content_block_delta",
                r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}"#,
            );
            meter.chunk(text.as_bytes(), ms(200));
            meter.chunk(text.as_bytes(), ms(last));
            meter.chunk(delta.as_bytes(), ms(last));
            meter.row()
        };
        let one = streamed(1, 700);
        assert_eq!(one.generation_tok_s(), None);
        assert_eq!(one.prefill_tok_s(), Some(2000.0));
        assert_eq!(streamed(2, 200).generation_tok_s(), None);
        assert_eq!(streamed(2, 700).generation_tok_s(), Some(2.0));
        // A prompt read wholly from the cache has no prefill speed.
        let mut meter = messages_meter();
        let cached = message_delta("end_turn").replace(
            r#""cache_creation_input_tokens": 399, "cache_read_input_tokens": 4800"#,
            r#""cache_creation_input_tokens": 0, "cache_read_input_tokens": 5199"#,
        );
        meter.chunk(cached.as_bytes(), ms(10));
        assert_eq!(meter.row().prefill_tok_s(), None);
    }

    #[test]
    fn images_embeddings_and_completions_make_rows_without_turns() {
        for (path, api) in [
            ("/v1/images/generations", RequestApi::Images),
            ("/v1/embeddings", RequestApi::Embeddings),
            ("/v1/completions", RequestApi::Completions),
            ("/images/generations", RequestApi::Images),
        ] {
            let mut meter = Meter::for_request(path, br#"{"prompt":"a cat"}"#).unwrap();
            meter
                .whole(br#"{"model": "Qwen-Image-2.1-Turbo", "data": [{"b64_json": "aGFsbG8="}]}"#);
            let row = meter.row();
            assert_eq!(row.api, api, "{path}");
            assert_eq!(row.start, None, "{path}");
            assert_eq!(row.answer, None, "{path}");
            assert_eq!(row.model.as_deref(), Some("Qwen-Image-2.1-Turbo"), "{path}");
        }
    }
}
