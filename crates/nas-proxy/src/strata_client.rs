//! The one place where the Proxy talks to Strata: `/health`, the metrics,
//! the update summary's chat request, and forwarding the clients' requests.
//! It knows Strata's address and key, which HTTP client each call takes and
//! how to read the answers (see `docs/strata-api.md`). It only asks and
//! reads; what an answer means for the PC is up to the caller.

use std::time::Duration;

use axum::body::Bytes;
use reqwest::{
    Client, Method, Response,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::Value;

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

/// Strata's numbers from `/metrics?format=prometheus`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub processing: u32,
    pub deferred: u32,
    pub tokens_per_sec: Option<f64>,
}

/// What a chat request answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatAnswer {
    /// The answer without the thinking; empty if there is none.
    pub content: String,
    /// The model that answered, if Strata names it.
    pub model: Option<String>,
}

/// Strata at the address of the current settings.
pub struct StrataClient {
    base: String,
    auth: Option<HeaderValue>,
    /// Short timeout, for `/health` and the metrics.
    probe: Client,
    /// No overall timeout, for answers that take long.
    upstream: Client,
}

impl StrataClient {
    /// `None` without a Strata address.
    pub fn new(cfg: &Config, probe: Client, upstream: Client) -> Option<Self> {
        Some(Self {
            base: cfg.strata_url.clone()?,
            auth: cfg
                .strata_api_key
                .as_deref()
                .and_then(|key| HeaderValue::from_str(&format!("Bearer {key}")).ok()),
            probe,
            upstream,
        })
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
            .request(&self.probe, Method::GET, "/health")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("/health answers {}", resp.status()));
        }
        Ok(parse_health(&resp.bytes().await.unwrap_or_default()))
    }

    pub async fn metrics(&self) -> Result<Metrics, String> {
        const NONE: &str = "no metrics from Strata";
        // Not `/metrics/prometheus`: its tokens/s counts finished requests only,
        // and each scrape resets that window for other scrapers.
        let resp = self
            .request(&self.probe, Method::GET, "/metrics?format=prometheus")
            .send()
            .await
            .map_err(|_| NONE)?;
        if !resp.status().is_success() {
            return Err(NONE.into());
        }
        parse_metrics(&resp.text().await.map_err(|_| NONE)?).ok_or_else(|| NONE.into())
    }

    /// `POST /v1/chat/completions` with `body`, waiting at most `limit`.
    pub async fn chat(&self, body: &Value, limit: Duration) -> Result<ChatAnswer, String> {
        let resp = self
            .request(&self.upstream, Method::POST, "/v1/chat/completions")
            .timeout(limit)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    format!("Strata did not answer within {} s", limit.as_secs_f64())
                } else {
                    e.to_string()
                }
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
    ) -> reqwest::Result<Response> {
        headers.remove(AUTHORIZATION);
        self.request(&self.upstream, method, path_and_query)
            .headers(headers)
            .body(body)
            .send()
            .await
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
