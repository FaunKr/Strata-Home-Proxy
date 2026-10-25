//! The public model name: what apps of an API key that hides the model see
//! instead of the model Strata runs. Requests get the running model's real
//! id, answers and the list of models get the public name. Only the `model`
//! fields change; error texts and every other field stay as Strata wrote
//! them. The image model keeps its own name.

use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use axum::body::Bytes;
use futures_util::Stream;
use serde_json::Value;

/// How the answers to one request name the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicName {
    /// The name apps see.
    pub public: String,
    /// The running model's real id, from Strata's latest `/health`; `None`
    /// while unknown.
    pub real: Option<String>,
}

/// Requests for the image model (`POST /v1/images/generations`) keep its
/// name, and so do their answers.
fn for_images(path: &str) -> bool {
    path.split('?')
        .next()
        .unwrap_or_default()
        .starts_with("/v1/images/")
}

impl PublicName {
    /// The request body for Strata: a top-level `model` becomes the real id.
    /// Unchanged while the real id is unknown (Strata serves any name with
    /// the model it runs), for the image model, and for a body that is not
    /// a JSON object with a `model`.
    pub fn request(&self, path: &str, body: Bytes) -> Bytes {
        let Some(real) = &self.real else {
            return body;
        };
        if for_images(path) {
            return body;
        }
        let Ok(Value::Object(mut map)) = serde_json::from_slice::<Value>(&body) else {
            return body;
        };
        match map.get_mut("model") {
            Some(model) if model.as_str() != Some(real) => *model = Value::from(real.as_str()),
            _ => return body,
        }
        Bytes::from(serde_json::to_vec(&map).expect("JSON serializes"))
    }

    /// A whole JSON answer to `path`, with the public name in its `model`
    /// fields. Anything that is not a JSON object passes unchanged.
    pub fn answer(&self, path: &str, body: Bytes) -> Bytes {
        if for_images(path) {
            return body;
        }
        self.rename(&body).map_or(body, Bytes::from)
    }

    /// The JSON object `text` with the public name in the fields an answer
    /// names the model in: top-level `model`, `message.model` (Anthropic's
    /// `message_start`) and `response.model` (Responses API events). `None`
    /// when nothing changes.
    fn rename(&self, text: &[u8]) -> Option<Vec<u8>> {
        let Ok(Value::Object(mut map)) = serde_json::from_slice::<Value>(text) else {
            return None;
        };
        let mut changed = self.set(map.get_mut("model"));
        for outer in ["message", "response"] {
            let inner = map
                .get_mut(outer)
                .and_then(Value::as_object_mut)
                .and_then(|o| o.get_mut("model"));
            changed |= self.set(inner);
        }
        changed.then(|| serde_json::to_vec(&map).expect("JSON serializes"))
    }

    /// Puts the public name into a string `model`; true if it changed.
    fn set(&self, model: Option<&mut Value>) -> bool {
        match model {
            Some(Value::String(name)) if *name != self.public => {
                *name = self.public.clone();
                true
            }
            _ => false,
        }
    }

    /// Strata's list of models as an app of this key sees it: the chat
    /// model and its aliases become one entry with the public name, the
    /// first of them with `aliases` and `alias_of` taken out. The image
    /// model, the only entry whose output is pictures, stays as it is,
    /// after it. A body that is not such a list passes unchanged.
    pub fn list(&self, body: &str) -> String {
        let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(body) else {
            return body.to_string();
        };
        let Some(Value::Array(data)) = map.get_mut("data") else {
            return body.to_string();
        };
        let (images, chat): (Vec<Value>, Vec<Value>) =
            std::mem::take(data).into_iter().partition(makes_pictures);
        let mut out = Vec::with_capacity(images.len() + 1);
        if let Some(Value::Object(mut first)) = chat.into_iter().next() {
            first.insert("id".into(), Value::from(self.public.as_str()));
            first.remove("aliases");
            first.remove("alias_of");
            out.push(Value::Object(first));
        }
        out.extend(images);
        *data = out;
        serde_json::to_string(&map).expect("JSON serializes")
    }

    /// An event stream with the public name in each event's `data`. Bytes
    /// are held back only up to the end of the event they belong to, so
    /// every chunk passed on ends between two events.
    pub fn stream<S, E>(&self, path: &str, upstream: S) -> Renamed<S>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        Renamed {
            upstream,
            name: (!for_images(path)).then(|| self.clone()),
            held: Vec::new(),
            ready: VecDeque::new(),
            done: false,
        }
    }

    /// One event with the public name in its `data` lines.
    fn event(&self, event: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(event.len());
        for line in event.split_inclusive(|&b| b == b'\n') {
            let body = line
                .strip_suffix(b"\n")
                .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
                .unwrap_or(line);
            let renamed = body.strip_prefix(b"data:").and_then(|data| {
                let space = usize::from(data.first() == Some(&b' '));
                self.rename(&data[space..])
                    .map(|json| [&body[..5 + space], &json[..], &line[body.len()..]].concat())
            });
            out.extend_from_slice(renamed.as_deref().unwrap_or(line));
        }
        out
    }
}

fn makes_pictures(entry: &Value) -> bool {
    entry["architecture"]["output_modalities"]
        .as_array()
        .is_some_and(|m| m.iter().any(|m| m == "image"))
}

/// The end of the first whole event in `bytes`: after its blank line.
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

/// See [`PublicName::stream`].
pub struct Renamed<S> {
    upstream: S,
    /// `None` for an answer that keeps its names (the image model).
    name: Option<PublicName>,
    /// The start of an event whose end has not come yet.
    held: Vec<u8>,
    ready: VecDeque<Bytes>,
    done: bool,
}

impl<S, E> Stream for Renamed<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(chunk) = this.ready.pop_front() {
                return Poll::Ready(Some(Ok(chunk)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            let Some(name) = &this.name else {
                return Pin::new(&mut this.upstream).poll_next(cx);
            };
            match Pin::new(&mut this.upstream).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(chunk))) => {
                    this.held.extend_from_slice(&chunk);
                    let mut out = Vec::new();
                    while let Some(end) = event_end(&this.held) {
                        let rest = this.held.split_off(end);
                        out.extend(name.event(&this.held));
                        this.held = rest;
                    }
                    if !out.is_empty() {
                        this.ready.push_back(Bytes::from(out));
                    }
                }
                Poll::Ready(None) => {
                    // A last event without its blank line passes as it is.
                    this.done = true;
                    if !this.held.is_empty() {
                        let last = std::mem::take(&mut this.held);
                        this.ready.push_back(Bytes::from(name.event(&last)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{StreamExt, stream};
    use serde_json::json;

    use super::*;

    fn hiding(real: Option<&str>) -> PublicName {
        PublicName {
            public: "strata".into(),
            real: real.map(String::from),
        }
    }

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    async fn streamed(name: &PublicName, path: &str, chunks: &[&str]) -> (Vec<String>, String) {
        let upstream = stream::iter(
            chunks
                .iter()
                .map(|c| Ok::<_, std::convert::Infallible>(Bytes::from(c.to_string())))
                .collect::<Vec<_>>(),
        );
        let out: Vec<String> = name
            .stream(path, upstream)
            .map(|c| String::from_utf8(c.unwrap().to_vec()).unwrap())
            .collect()
            .await;
        let all = out.concat();
        (out, all)
    }

    #[test]
    fn a_request_names_the_running_model() {
        let body = Bytes::from_static(br#"{"model":"strata","stream":true,"temperature":0.7}"#);
        let out = hiding(Some("coder-IQ1_M")).request("/v1/chat/completions", body);
        assert_eq!(
            json(&out),
            json!({"model": "coder-IQ1_M", "stream": true, "temperature": 0.7})
        );
    }

    #[test]
    fn a_request_is_unchanged_while_the_real_id_is_unknown() {
        let body = Bytes::from_static(br#"{"model":"strata"}"#);
        assert_eq!(
            hiding(None).request("/v1/chat/completions", body.clone()),
            body
        );
    }

    #[test]
    fn an_image_request_keeps_its_model() {
        let body = Bytes::from_static(br#"{"model":"flux-schnell","prompt":"a cat"}"#);
        let out = hiding(Some("IQ3_XXS")).request("/v1/images/generations", body.clone());
        assert_eq!(out, body);
    }

    #[test]
    fn a_request_without_a_model_or_json_is_unchanged() {
        let name = hiding(Some("IQ3_XXS"));
        let none = Bytes::from_static(br#"{"messages":[]}"#);
        assert_eq!(name.request("/v1/chat/completions", none.clone()), none);
        let text = Bytes::from_static(b"not json");
        assert_eq!(name.request("/v1/chat/completions", text.clone()), text);
    }

    #[test]
    fn an_answer_names_the_public_name() {
        let body = Bytes::from_static(
            br#"{"id":"c1","object":"chat.completion","model":"IQ3_XXS","choices":[{"message":{"content":"IQ3_XXS here"}}]}"#,
        );
        let out = hiding(Some("IQ3_XXS")).answer("/v1/chat/completions", body);
        let v = json(&out);
        assert_eq!(v["model"], "strata");
        // Only the field, not text that happens to hold the name.
        assert_eq!(v["choices"][0]["message"]["content"], "IQ3_XXS here");
    }

    #[test]
    fn an_answer_that_is_not_json_is_unchanged() {
        let body = Bytes::from_static(b"# HELP x\nx{model=\"IQ3_XXS\"} 1\n");
        assert_eq!(hiding(None).answer("/metrics", body.clone()), body);
    }

    #[test]
    fn the_list_has_one_chat_model_with_the_public_name() {
        let body = json!({"object": "list", "data": [
            {"id": "IQ3_XXS", "object": "model", "status": {"value": "loaded"},
             "meta": {"n_ctx": 32768}, "aliases": ["qwen"],
             "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}},
            {"id": "qwen", "object": "model", "alias_of": "IQ3_XXS", "aliases": ["qwen"],
             "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}},
            {"id": "flux-schnell", "object": "model", "status": {"value": "loaded"},
             "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]}}
        ]})
        .to_string();
        let out = hiding(None).list(&body);
        assert_eq!(
            json(out.as_bytes()),
            json!({"object": "list", "data": [
                {"id": "strata", "object": "model", "status": {"value": "loaded"},
                 "meta": {"n_ctx": 32768},
                 "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}},
                {"id": "flux-schnell", "object": "model", "status": {"value": "loaded"},
                 "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]}}
            ]})
        );
    }

    #[test]
    fn a_list_with_only_the_image_model_stays() {
        let body = json!({"object": "list", "data": [
            {"id": "flux-schnell", "architecture": {"output_modalities": ["image"]}}
        ]})
        .to_string();
        assert_eq!(
            json(hiding(None).list(&body).as_bytes()),
            json(body.as_bytes())
        );
    }

    #[tokio::test]
    async fn every_streamed_event_names_the_public_name() {
        let name = hiding(None);
        let (_, all) = streamed(
            &name,
            "/v1/chat/completions",
            &[
                "data: {\"id\":\"c\",\"model\":\"IQ3_XXS\",\"choices\":[]}\n\n",
                "data: {\"id\":\"c\",\"model\":\"IQ3_XXS\",\"choices\":[]}\n\ndata: [DONE]\n\n",
            ],
        )
        .await;
        let events: Vec<&str> = all.split_terminator("\n\n").collect();
        assert_eq!(events.len(), 3, "{all}");
        for event in &events[..2] {
            let data = event.strip_prefix("data: ").unwrap();
            assert_eq!(
                json(data.as_bytes()),
                json!({"id": "c", "model": "strata", "choices": []})
            );
        }
        assert_eq!(events[2], "data: [DONE]");
    }

    #[tokio::test]
    async fn anthropic_and_responses_events_name_the_public_name() {
        let (_, all) = streamed(
            &hiding(None),
            "/v1/messages",
            &[
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"IQ3_XXS\"}}\n\n",
                "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"IQ3_XXS\"}}\n\n",
            ],
        )
        .await;
        assert!(!all.contains("IQ3_XXS"), "{all}");
        assert_eq!(all.matches("\"model\":\"strata\"").count(), 2, "{all}");
        assert!(all.starts_with("event: message_start\ndata: {"), "{all}");
    }

    #[tokio::test]
    async fn an_event_split_over_chunks_passes_once_it_is_whole() {
        let (chunks, all) = streamed(
            &hiding(None),
            "/v1/chat/completions",
            &[
                "data: {\"mod",
                "el\":\"IQ3_XXS\"}\n",
                "\ndata: {\"model\":\"IQ3_XXS\"}\n\n",
            ],
        )
        .await;
        assert_eq!(
            all,
            "data: {\"model\":\"strata\"}\n\ndata: {\"model\":\"strata\"}\n\n"
        );
        // Every chunk passed on ends between two events.
        assert!(chunks.iter().all(|c| c.ends_with("\n\n")), "{chunks:?}");
    }

    #[tokio::test]
    async fn crlf_events_keep_their_line_ends() {
        let (_, all) = streamed(
            &hiding(None),
            "/v1/chat/completions",
            &["data: {\"model\":\"IQ3_XXS\"}\r\n\r\n"],
        )
        .await;
        assert_eq!(all, "data: {\"model\":\"strata\"}\r\n\r\n");
    }

    #[tokio::test]
    async fn an_unfinished_last_event_still_passes() {
        let (_, all) = streamed(
            &hiding(None),
            "/v1/chat/completions",
            &[
                "data: {\"model\":\"IQ3_XXS\"}\n\n",
                "data: {\"model\":\"IQ3_XXS\"}",
            ],
        )
        .await;
        assert_eq!(
            all,
            "data: {\"model\":\"strata\"}\n\ndata: {\"model\":\"strata\"}"
        );
    }

    #[tokio::test]
    async fn an_image_stream_passes_unchanged() {
        let chunk = "data: {\"model\":\"flux-schnell\"}\n\n";
        let (_, all) = streamed(&hiding(None), "/v1/images/generations", &[chunk]).await;
        assert_eq!(all, chunk);
    }
}
