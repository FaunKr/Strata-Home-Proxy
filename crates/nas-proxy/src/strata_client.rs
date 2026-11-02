//! The one place where the Proxy talks to Strata: `/health`, the metrics,
//! the update summary's chat request, the list of models after a wake-up,
//! and forwarding the clients' requests.
//! It knows Strata's address and key, which HTTP client each call takes and
//! how to read the answers (see `docs/strata-api.md`). In relay mode it asks
//! the PC service's relay instead, with the PC service's token; no caller
//! learns which way a call went. It only asks and reads; what an answer means
//! for the PC is up to the caller.

use std::time::Duration;

use axum::body::Bytes;
use reqwest::{
    Client, Method, Response,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::Value;

use strata_pc_protocol::{RELAY_NO_STRATA, RELAY_PATH};

use crate::config::Config;

/// Strata's `/health` as the Proxy reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrataHealth {
    /// False while the model is not in memory; Strata loads it with the next
    /// request. Counts as true when the answer does not say.
    pub loaded: bool,
    pub model: Option<String>,
    pub max_context: Option<u64>,
}

const NO_METRICS: &str = "no metrics from Strata";

/// Strata's numbers from `/metrics?format=prometheus`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub processing: u32,
    pub deferred: u32,
    pub tokens_per_sec: Option<f64>,
}

/// One of Strata's finished requests (`/metrics?requests=all`), with the
/// fields the expert tiers read (docs/strata-api.md).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct StrataRequest {
    /// Strata's own id: random, never sent to the client.
    pub id: String,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Decode's expert cache hits over lookups.
    #[serde(default)]
    pub hit_rate: Option<f64>,
    /// Offloaded experts over lookups plus offloaded.
    #[serde(default)]
    pub pcie_share: Option<f64>,
}

/// What a chat request answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatAnswer {
    /// The answer without the thinking; empty if there is none.
    pub content: String,
    /// The model that answered, if Strata names it.
    pub model: Option<String>,
}

/// Strata at the address of the current settings, or through the PC
/// service's relay.
pub struct StrataClient {
    base: String,
    auth: Option<HeaderValue>,
    /// Through the relay: its marked `502` is no answer of Strata's.
    relayed: bool,
    /// Short timeout, for `/health` and the metrics.
    probe: Client,
    /// No overall timeout, for answers that take long.
    upstream: Client,
}

impl StrataClient {
    /// `None` without a Strata address. With `relay`, and the PC service's
    /// address set, the calls go through its relay with its token, and `key`
    /// is not used: the PC service adds the Strata key.
    pub fn new(
        cfg: &Config,
        key: Option<&str>,
        relay: bool,
        probe: Client,
        upstream: Client,
    ) -> Option<Self> {
        let direct = cfg.strata_url.clone()?;
        let bearer = |secret: &str| HeaderValue::from_str(&format!("Bearer {secret}")).ok();
        let (base, auth, relayed) = match (&cfg.pc_agent_url, relay) {
            (Some(agent), true) => (
                format!("{agent}{RELAY_PATH}"),
                cfg.pc_agent_token.as_deref().and_then(bearer),
                true,
            ),
            _ => (direct, key.and_then(bearer), false),
        };
        Some(Self {
            base,
            auth,
            relayed,
            probe,
            upstream,
        })
    }

    /// Sends `req`; the relay's marked `502` counts as no answer, like a
    /// failed connection.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Response, Unanswered> {
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                Unanswered::TimedOut
            } else {
                Unanswered::Failed(e.to_string())
            }
        })?;
        if self.relayed && resp.headers().contains_key(RELAY_NO_STRATA) {
            return Err(Unanswered::Failed(
                "Strata does not answer the PC service".into(),
            ));
        }
        Ok(resp)
    }

    /// A request with Strata's key, when one is set.
    fn request(&self, http: &Client, method: Method, path: &str) -> reqwest::RequestBuilder {
        let req = http.request(method, format!("{}{path}", self.base));
        match &self.auth {
            Some(auth) => req.header(AUTHORIZATION, auth.clone()),
            None => req,
        }
    }

    pub async fn health(&self) -> Result<StrataHealth, String> {
        let resp = self
            .send(self.request(&self.probe, Method::GET, "/health"))
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("/health answers {}", resp.status()));
        }
        Ok(parse_health(&resp.bytes().await.unwrap_or_default()))
    }

    pub async fn metrics(&self) -> Result<Metrics, String> {
        parse_metrics(&self.metrics_text().await?).ok_or_else(|| NO_METRICS.into())
    }

    /// Strata's metrics as Prometheus text, unchanged: for the Overview and
    /// the proxy's own metrics endpoint.
    pub async fn metrics_text(&self) -> Result<String, String> {
        // Not `/metrics/prometheus`: its tokens/s counts finished requests only,
        // and each read resets that window (docs/strata-api.md).
        self.prometheus_text("/metrics?format=prometheus").await
    }

    /// Strata's `/metrics/prometheus` (`llamacpp:*` names) as text, unchanged:
    /// only for the tool-call counters of the proxy's own metrics endpoint,
    /// which Strata prints nowhere else.
    pub async fn metrics_prometheus_text(&self) -> Result<String, String> {
        self.prometheus_text("/metrics/prometheus").await
    }

    /// The body of a `200` to `GET path`, with the probe timeout.
    async fn prometheus_text(&self, path: &str) -> Result<String, String> {
        let resp = self
            .send(self.request(&self.probe, Method::GET, path))
            .await
            .map_err(|_| NO_METRICS)?;
        if !resp.status().is_success() {
            return Err(NO_METRICS.into());
        }
        resp.text().await.map_err(|_| NO_METRICS.into())
    }

    /// Strata's last finished requests, newest first.
    pub async fn finished_requests(&self) -> Result<Vec<StrataRequest>, String> {
        const NO_REQUESTS: &str = "no finished requests from Strata";
        let resp = self
            .send(self.request(&self.probe, Method::GET, "/metrics?requests=all"))
            .await
            .map_err(|_| NO_REQUESTS)?;
        if !resp.status().is_success() {
            return Err(NO_REQUESTS.into());
        }
        let body = resp.bytes().await.map_err(|_| NO_REQUESTS)?;
        parse_finished_requests(&body).ok_or_else(|| NO_REQUESTS.into())
    }

    /// `GET /v1/models`: its `Content-Type` and body, for a `200` only.
    pub async fn models(&self) -> Result<(Option<String>, Bytes), String> {
        let resp = self
            .send(self.request(&self.probe, Method::GET, "/v1/models"))
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("/v1/models answers {}", resp.status()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = resp.bytes().await.map_err(|e| e.to_string())?;
        Ok((content_type, body))
    }

    /// `POST /v1/chat/completions` with `body`, waiting at most `limit`.
    pub async fn chat(&self, body: &Value, limit: Duration) -> Result<ChatAnswer, String> {
        let req = self
            .request(&self.upstream, Method::POST, "/v1/chat/completions")
            .timeout(limit)
            .json(body);
        let resp = self.send(req).await.map_err(|e| match e {
            Unanswered::TimedOut => {
                format!("Strata did not answer within {} s", limit.as_secs_f64())
            }
            Unanswered::Failed(e) => e,
        })?;
        if !resp.status().is_success() {
            return Err(format!("Strata answers {}", resp.status()));
        }
        let answer: Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(ChatAnswer {
            content: answer["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            model: answer["model"].as_str().map(String::from),
        })
    }

    /// Passes a client's request on with Strata's key. Clearing the client's
    /// credentials is the forwarding code's job; an `authorization` left in
    /// `headers` is dropped all the same, as it would replace the key. The
    /// answer comes back untouched, for streaming.
    pub async fn forward(
        &self,
        method: Method,
        path_and_query: &str,
        mut headers: HeaderMap,
        body: Bytes,
    ) -> Result<Response, String> {
        headers.remove(AUTHORIZATION);
        let req = self
            .request(&self.upstream, method, path_and_query)
            .headers(headers)
            .body(body);
        self.send(req).await.map_err(|e| e.to_string())
    }
}

/// Why a call to Strata got no answer.
enum Unanswered {
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("Strata did not answer in time"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

/// Reads a `/health` answer; one that is not JSON, or lacks `loaded`, counts
/// as loaded, since only builds other than Strata answer that way.
pub fn parse_health(body: &[u8]) -> StrataHealth {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default = "loaded_default")]
        loaded: bool,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        max_context: Option<u64>,
    }
    fn loaded_default() -> bool {
        true
    }
    match serde_json::from_slice::<Body>(body) {
        Ok(b) => StrataHealth {
            loaded: b.loaded,
            model: b.model,
            max_context: b.max_context,
        },
        Err(_) => StrataHealth {
            loaded: true,
            model: None,
            max_context: None,
        },
    }
}

/// The `requests` of Strata's JSON `/metrics`; `None` without them.
pub fn parse_finished_requests(body: &[u8]) -> Option<Vec<StrataRequest>> {
    #[derive(Deserialize)]
    struct Answer {
        requests: Vec<StrataRequest>,
    }
    serde_json::from_slice::<Answer>(body)
        .ok()
        .map(|a| a.requests)
}

/// Reads the few values the Proxy needs from Prometheus text.
pub fn parse_metrics(text: &str) -> Option<Metrics> {
    let value = |name: &str| {
        text.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
            let (key, value) = l.split_once(' ')?;
            // Without or with labels, e.g. `name{model="x"} 1`.
            let key = key.split('{').next()?;
            (key == name).then(|| value.trim().parse::<f64>().ok())?
        })
    };
    let processing = value("vllm:num_requests_running")?;
    Some(Metrics {
        processing: processing as u32,
        deferred: value("vllm:num_requests_waiting").unwrap_or(0.0) as u32,
        tokens_per_sec: value("strata:live_tok_s").filter(|v| *v > 0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stratas_finished_requests() {
        // Every key of Strata c62d5e3a's `history.append`, docs/strata-api.md.
        assert_eq!(
            parse_finished_requests(include_bytes!(
                "../tests/fixtures/strata-finished-requests.json"
            )),
            Some(vec![StrataRequest {
                id: "3f9c2a7b1e04".into(),
                output_tokens: Some(212),
                hit_rate: Some(0.874),
                pcie_share: Some(0.062),
            }])
        );
        assert_eq!(parse_finished_requests(b"{}"), None);
    }

    #[test]
    fn reads_strata_metrics() {
        // Real output of Strata's serve/prometheus.py, see docs/strata-api.md.
        assert_eq!(
            parse_metrics(include_str!("../tests/fixtures/strata-metrics-busy.prom")),
            Some(Metrics {
                processing: 1,
                deferred: 1,
                tokens_per_sec: Some(38.5),
            })
        );
        assert_eq!(
            parse_metrics(include_str!("../tests/fixtures/strata-metrics-idle.prom")),
            Some(Metrics::default())
        );
        assert_eq!(parse_metrics("other 1\n"), None);
    }

    #[test]
    fn reads_health_as_strata_sends_it() {
        let body = br#"{"status": "ok", "max_context": 32768, "model": "IQ3_XXS", "loaded": false, "service": "strata"}"#;
        assert_eq!(
            parse_health(body),
            StrataHealth {
                loaded: false,
                model: Some("IQ3_XXS".into()),
                max_context: Some(32768),
            }
        );
    }

    #[test]
    fn health_without_loaded_or_json_counts_as_loaded() {
        let unknown = StrataHealth {
            loaded: true,
            model: None,
            max_context: None,
        };
        assert_eq!(parse_health(br#"{"status": "ok"}"#), unknown);
        assert_eq!(parse_health(b"OK"), unknown);
        assert_eq!(parse_health(b""), unknown);
    }

    #[test]
    fn health_with_some_fields_keeps_those() {
        assert_eq!(
            parse_health(br#"{"loaded": false}"#),
            StrataHealth {
                loaded: false,
                model: None,
                max_context: None,
            }
        );
        assert_eq!(
            parse_health(br#"{"model": "IQ3_XXS"}"#),
            StrataHealth {
                loaded: true,
                model: Some("IQ3_XXS".into()),
                max_context: None,
            }
        );
    }
}
