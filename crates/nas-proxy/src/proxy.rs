//! Forwarding of the Strata API (OpenAI and Anthropic format).

use std::{
    io,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{Stream, StreamExt, stream};
use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    events::EventKind,
    state::{ActivityGuard, AppState, Readiness, WakeError},
};

/// Header that tells clients (e.g. the `local-agent` skill) why Strata is not
/// available: `gaming`, `wake_timeout`, `no_mac`, `not_configured`, or
/// `unauthorized` for a missing or wrong API key, or `strata_unauthorized`
/// when Strata refuses the proxy's Strata key.
pub const STATUS_HEADER: &str = "x-strata-proxy-status";

/// Which API format a request uses; decides how pings and errors are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Anthropic,
    OpenAi,
}

impl Flavor {
    pub fn from_path(path: &str) -> Self {
        if path.contains("/messages") {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    /// Keeps the connection alive without changing the answer. Anthropic clients
    /// skip `ping` events; OpenAI clients skip SSE comments.
    fn ping(self) -> Bytes {
        match self {
            Self::Anthropic => Bytes::from_static(b"event: ping\ndata: {\"type\": \"ping\"}\n\n"),
            Self::OpenAi => Bytes::from_static(b": ping\n\n"),
        }
    }

    fn error_json(self, kind: &str, message: &str) -> serde_json::Value {
        match self {
            Self::Anthropic => json!({
                "type": "error",
                "error": { "type": kind, "message": message },
            }),
            Self::OpenAi => json!({
                "error": { "type": kind, "message": message, "code": kind },
            }),
        }
    }

    fn error_event(self, kind: &str, message: &str) -> Bytes {
        let data = self.error_json(kind, message);
        Bytes::from(match self {
            Self::Anthropic => format!("event: error\ndata: {data}\n\n"),
            Self::OpenAi => format!("data: {data}\n\n"),
        })
    }
}

const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

fn copy_headers(from: &HeaderMap, drop_length: bool) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in from {
        let n = name.as_str();
        if HOP_BY_HOP.contains(&n) || (drop_length && n == "content-length") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Credentials of the client. They are meant for the proxy and never reach
/// Strata, which gets the proxy's own key instead.
const CLIENT_CREDENTIALS: [&str; 2] = ["authorization", "x-api-key"];

/// The request headers for Strata: the client's, minus hop-by-hop headers and
/// its credentials. The Strata client adds the Proxy's key.
fn upstream_headers(from: &HeaderMap) -> HeaderMap {
    let mut out = copy_headers(from, true);
    for name in CLIENT_CREDENTIALS {
        out.remove(name);
    }
    out
}

fn is_streaming(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false)
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

/// Status, header tag, error type and message for a request that cannot be served.
fn wake_error_parts(err: &WakeError) -> (StatusCode, &'static str, &'static str, &'static str) {
    match err {
        WakeError::Gaming => (
            StatusCode::SERVICE_UNAVAILABLE,
            "gaming",
            "overloaded_error",
            "Strata is not available: gaming mode is on.",
        ),
        WakeError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "wake_timeout",
            "api_error",
            "Strata is not available: the PC did not wake in time.",
        ),
        WakeError::NoMac => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no_mac",
            "api_error",
            "Strata is not available: the PC's MAC address is not known, so it cannot be woken.",
        ),
    }
}

fn tagged(mut resp: Response, tag: &'static str) -> Response {
    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static(STATUS_HEADER),
        HeaderValue::from_static(tag),
    );
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("300"));
    resp
}

/// The API keys a client sent: `x-api-key` (Anthropic) and a bearer token in
/// `authorization` (OpenAI). Some clients send both, one of them stale, so
/// either may hold the valid key.
fn client_keys(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    let x_api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().split_once(' '))
        .and_then(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer").then(|| token.trim()));
    [x_api_key, bearer]
        .into_iter()
        .flatten()
        .filter(|k| !k.is_empty())
}

/// Answer for a missing or unknown API key. It comes before anything else, so
/// such a request never wakes the PC or counts as activity.
fn unauthorized(flavor: Flavor) -> Response {
    const MESSAGE: &str = "No valid API key. Keys are in the proxy's web UI under Access.";
    let body = match flavor {
        Flavor::Anthropic => flavor.error_json("authentication_error", MESSAGE),
        // What OpenAI itself answers for a wrong key.
        Flavor::OpenAi => json!({
            "error": { "type": "invalid_request_error", "message": MESSAGE, "code": "invalid_api_key" },
        }),
    };
    let mut resp = (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        HeaderName::from_static(STATUS_HEADER),
        HeaderValue::from_static("unauthorized"),
    );
    resp
}

/// Answer while no Strata URL is set.
fn not_configured(flavor: Flavor) -> Response {
    let body = flavor.error_json(
        "api_error",
        "The proxy is not set up: Strata's address is missing (STRATA_URL, or in the web UI under Strata → Connection).",
    );
    tagged(
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response(),
        "not_configured",
    )
}

fn unavailable(flavor: Flavor, err: &WakeError) -> Response {
    let (status, tag, kind, message) = wake_error_parts(err);
    tagged(
        (status, axum::Json(flavor.error_json(kind, message))).into_response(),
        tag,
    )
}

struct Incoming {
    method: reqwest::Method,
    path_and_query: String,
    headers: HeaderMap,
    body: Bytes,
}

async fn send_upstream(st: &AppState, req: &Incoming) -> Result<reqwest::Response, String> {
    // Checked before forwarding; only fails if the address was unset since.
    let strata = st.strata().ok_or("STRATA_URL is not set")?;
    strata
        .forward(
            req.method.clone(),
            &req.path_and_query,
            upstream_headers(&req.headers),
            req.body.clone(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Passes upstream chunks through and inserts a ping whenever the stream has
/// been silent for `interval` and sits between two events.
fn with_pings(
    upstream: impl Stream<Item = reqwest::Result<Bytes>> + Send + Unpin + 'static,
    flavor: Flavor,
    interval: Duration,
) -> impl Stream<Item = reqwest::Result<Bytes>> + Send {
    stream::unfold(
        (upstream, true),
        move |(mut upstream, mut at_boundary)| async move {
            loop {
                match tokio::time::timeout(interval, upstream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        if !chunk.is_empty() {
                            at_boundary = chunk.ends_with(b"\n\n") || chunk.ends_with(b"\r\n\r\n");
                        }
                        return Some((Ok(chunk), (upstream, at_boundary)));
                    }
                    Ok(Some(Err(e))) => return Some((Err(e), (upstream, at_boundary))),
                    Ok(None) => return None,
                    Err(_) if at_boundary => {
                        return Some((Ok(flavor.ping()), (upstream, at_boundary)));
                    }
                    Err(_) => continue,
                }
            }
        },
    )
}

/// What a client reads when its request is given up as lost.
const LOST: &str = "Strata is no longer working on the request.";

fn lost(flavor: Flavor) -> Response {
    (
        StatusCode::GATEWAY_TIMEOUT,
        axum::Json(flavor.error_json("api_error", LOST)),
    )
        .into_response()
}

fn upstream_failed(flavor: Flavor, e: &str) -> Response {
    tracing::warn!("Forwarding to Strata failed: {e}");
    (
        StatusCode::BAD_GATEWAY,
        axum::Json(flavor.error_json("api_error", "Strata did not answer.")),
    )
        .into_response()
}

/// Forwards a request to a Strata that is known to be up and passes the answer
/// through unchanged, adding pings to event streams.
async fn forward(
    st: Arc<AppState>,
    guard: ActivityGuard,
    req: Incoming,
    flavor: Flavor,
) -> Response {
    let mut given_up = Box::pin(guard.given_up(Instant::now()));
    let sent = tokio::select! {
        sent = send_upstream(&st, &req) => sent,
        () = &mut given_up => return lost(flavor),
    };
    let resp = match sent {
        Ok(r) => r,
        Err(e) => return upstream_failed(flavor, &e),
    };
    let status = resp.status();
    let sse = is_event_stream(resp.headers());
    let headers = copy_headers(resp.headers(), sse);
    let ping_interval = st.cfg().ping_interval;
    let body = resp.bytes_stream();
    let body = if sse {
        let tail = flavor.error_event("api_error", LOST);
        Body::from_stream(keep_guard(
            with_pings(body, flavor, ping_interval),
            guard,
            given_up,
            Some(tail),
        ))
    } else {
        Body::from_stream(keep_guard(body, guard, given_up, None))
    };
    let mut out = Response::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    if status == StatusCode::UNAUTHORIZED {
        // The client's key was fine; Strata refused the proxy's own key.
        strata_refused(&st);
        out.headers_mut().insert(
            HeaderName::from_static(STATUS_HEADER),
            HeaderValue::from_static("strata_unauthorized"),
        );
    }
    out
}

/// Logs that Strata refused the proxy's key, or that it lacks one, and
/// returns the same text for the client.
fn strata_refused(st: &AppState) -> &'static str {
    let message = if st.cfg().strata_api_key.is_some() {
        "Strata refuses the Strata key: check STRATA_API_KEY."
    } else {
        "Strata requires a key: set STRATA_API_KEY in the settings."
    };
    tracing::warn!("{message}");
    message
}

/// Holds the activity guard until the body stream ends or is dropped. A
/// request given up as lost ends with `tail` (an error event), or without
/// one with an error, so the client never takes a cut answer for a whole one.
fn keep_guard<S, G>(
    inner: S,
    guard: ActivityGuard,
    given_up: Pin<Box<G>>,
    tail: Option<Bytes>,
) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    G: Future<Output = ()> + Send + 'static,
{
    let inner = Box::pin(inner);
    stream::unfold(Some((inner, given_up, guard, tail)), |held| async move {
        let (mut inner, mut given_up, guard, tail) = held?;
        tokio::select! {
            item = inner.next() => {
                let item = item?.map_err(io::Error::other);
                Some((item, Some((inner, given_up, guard, tail))))
            }
            () = &mut given_up => {
                let end = tail.ok_or_else(|| io::Error::other(LOST));
                Some((end, None))
            }
        }
    })
}

/// For streamed requests while the PC sleeps: answers with an event stream at
/// once and sends pings while it wakes, so clients do not give up (Claude Code
/// stops after 300 s without data). Then the real answer follows.
fn wake_then_stream(
    st: Arc<AppState>,
    guard: ActivityGuard,
    req: Incoming,
    flavor: Flavor,
) -> Response {
    let (tx, mut rx) = mpsc::channel::<Bytes>(16);
    tokio::spawn(wake_and_pass_on(st, guard, req, flavor, tx));
    let body = stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Ok::<_, std::convert::Infallible>);
    let mut resp = Response::new(Body::from_stream(body));
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// The task behind `wake_then_stream`: pings while the PC wakes, then the answer.
async fn wake_and_pass_on(
    st: Arc<AppState>,
    guard: ActivityGuard,
    req: Incoming,
    flavor: Flavor,
    tx: mpsc::Sender<Bytes>,
) {
    if tx.send(flavor.ping()).await.is_err() {
        return;
    }
    let wake = st.ensure_awake();
    tokio::pin!(wake);
    let woke = loop {
        tokio::select! {
            r = &mut wake => break r,
            _ = tokio::time::sleep(st.cfg().ping_interval) => {
                if tx.send(flavor.ping()).await.is_err() {
                    return;
                }
            }
        }
    };
    if let Err(err) = woke {
        if err == WakeError::Gaming {
            st.events.record(EventKind::GamingRejected, None);
        }
        let (_, _, kind, message) = wake_error_parts(&err);
        let _ = tx.send(flavor.error_event(kind, message)).await;
        return;
    }
    let given_up = guard.given_up(Instant::now());
    tokio::select! {
        () = pass_on(&st, &req, flavor, &tx) => {}
        () = given_up => {
            let _ = tx.send(flavor.error_event("api_error", LOST)).await;
        }
    }
}

/// Sends the request to Strata and passes its event stream on.
async fn pass_on(st: &AppState, req: &Incoming, flavor: Flavor, tx: &mpsc::Sender<Bytes>) {
    let resp = match send_upstream(st, req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Forwarding to Strata failed: {e}");
            let _ = tx
                .send(flavor.error_event("api_error", "Strata did not answer."))
                .await;
            return;
        }
    };
    if resp.status() == StatusCode::UNAUTHORIZED {
        let message = strata_refused(st);
        let _ = tx.send(flavor.error_event("api_error", message)).await;
        return;
    }
    if !resp.status().is_success() || !is_event_stream(resp.headers()) {
        // The status line is already sent; pass the error on as an event.
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let message = format!("Strata answered with {status}: {text}");
        let _ = tx.send(flavor.error_event("api_error", &message)).await;
        return;
    }
    let mut body = Box::pin(with_pings(
        resp.bytes_stream(),
        flavor,
        st.cfg().ping_interval,
    ));
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(chunk) => {
                if tx.send(chunk).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!("Answer from Strata cut off: {e}");
                return;
            }
        }
    }
}

pub async fn handle(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let flavor = Flavor::from_path(parts.uri.path());
    if !client_keys(&parts.headers).any(|key| st.api_keys.check(key).is_some()) {
        return unauthorized(flavor);
    }
    if st.strata().is_none() {
        return not_configured(flavor);
    }
    let cfg = st.cfg();
    let guard = ActivityGuard::new(&st);
    let body = match axum::body::to_bytes(body, cfg.max_body_bytes).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                axum::Json(flavor.error_json("invalid_request_error", "Request is too large.")),
            )
                .into_response();
        }
    };
    let streaming = is_streaming(&body);
    let req = Incoming {
        method: parts.method,
        path_and_query: parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".to_string(), |p| p.as_str().to_string()),
        headers: parts.headers,
        body,
    };
    match st.readiness().await {
        Readiness::Ready => forward(st, guard, req, flavor).await,
        Readiness::Gaming => {
            st.events.record(EventKind::GamingRejected, None);
            unavailable(flavor, &WakeError::Gaming)
        }
        Readiness::NeedsWake if streaming => wake_then_stream(st, guard, req, flavor),
        Readiness::NeedsWake => match st.ensure_awake().await {
            Ok(()) => forward(st, guard, req, flavor).await,
            Err(err) => {
                if err == WakeError::Gaming {
                    st.events.record(EventKind::GamingRejected, None);
                }
                unavailable(flavor, &err)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_flavor_and_streaming() {
        assert_eq!(Flavor::from_path("/v1/messages"), Flavor::Anthropic);
        assert_eq!(Flavor::from_path("/v1/chat/completions"), Flavor::OpenAi);
        assert!(is_streaming(br#"{"stream": true, "model": "x"}"#));
        assert!(!is_streaming(br#"{"stream": false}"#));
        assert!(!is_streaming(b"not json"));
    }
}
