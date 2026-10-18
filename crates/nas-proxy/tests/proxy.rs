//! End-to-end tests against a simulated PC: Strata, the PC service and a Wake
//! on LAN listener, all on localhost.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde_json::{Value, json};
use strata_nas_proxy::{
    AppState, Config, api_router, events::EventKind, proxy::STATUS_HEADER, state::PcState,
    wol::magic_packet,
};
use tokio::net::{TcpListener, UdpSocket};

const MAC: [u8; 6] = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];

#[derive(Default)]
struct FakePc {
    awake: AtomicBool,
    gaming: AtomicBool,
    refuse_sleep: AtomicBool,
    ignore_wol: AtomicBool,
    wol_packets: AtomicUsize,
    sleep_requests: AtomicUsize,
    api_requests: AtomicUsize,
}

impl FakePc {
    fn up(&self) -> bool {
        self.awake.load(Ordering::SeqCst)
    }
    fn strata_up(&self) -> bool {
        self.up() && !self.gaming.load(Ordering::SeqCst)
    }
}

type Pc = Arc<FakePc>;

async fn health(State(pc): State<Pc>) -> StatusCode {
    if pc.strata_up() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// An event stream with a pause in the middle, like a model thinking.
fn sse(events: Vec<String>, pause_after_first: Duration) -> Response {
    let s = stream::unfold(
        (events.into_iter(), 0usize),
        move |(mut it, i)| async move {
            let ev = it.next()?;
            if i == 1 {
                tokio::time::sleep(pause_after_first).await;
            }
            Some((
                Ok::<_, std::convert::Infallible>(Bytes::from(ev)),
                (it, i + 1),
            ))
        },
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(s))
        .unwrap()
}

async fn messages(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
    if !pc.strata_up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.api_requests.fetch_add(1, Ordering::SeqCst);
    let pause = Duration::from_millis(body["pause_ms"].as_u64().unwrap_or(0));
    if body["stream"].as_bool() == Some(true) {
        return sse(
            vec![
                "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".into(),
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"Hallo\"}}\n\n".into(),
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".into(),
            ],
            pause,
        );
    }
    Json(json!({"type": "message", "content": [{"type": "text", "text": "Hallo"}]})).into_response()
}

async fn chat(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
    if !pc.strata_up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.api_requests.fetch_add(1, Ordering::SeqCst);
    if body["stream"].as_bool() == Some(true) {
        return sse(
            vec![
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hallo\"}}]}\n\n".into(),
                "data: [DONE]\n\n".into(),
            ],
            Duration::ZERO,
        );
    }
    Json(json!({"choices": [{"message": {"content": "Hallo"}}]})).into_response()
}

async fn agent_status(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        // A sleeping PC does not answer; a refused status is the closest stand-in.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({
        "gaming_mode": pc.gaming.load(Ordering::SeqCst),
        "strata_running": pc.strata_up(),
    }))
    .into_response()
}

async fn agent_sleep(State(pc): State<Pc>) -> Response {
    pc.sleep_requests.fetch_add(1, Ordering::SeqCst);
    if pc.refuse_sleep.load(Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"reason": "Benutzer aktiv"})),
        )
            .into_response();
    }
    pc.awake.store(false, Ordering::SeqCst);
    Json(json!({"accepted": true})).into_response()
}

async fn serve(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// Listens for magic packets and wakes the fake PC shortly after one arrives.
async fn wol_listener(pc: Pc) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 256];
        loop {
            let (n, _) = socket.recv_from(&mut buf).await.unwrap();
            if buf[..n] == magic_packet(MAC) {
                pc.wol_packets.fetch_add(1, Ordering::SeqCst);
                if !pc.ignore_wol.load(Ordering::SeqCst) {
                    let pc = pc.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        pc.awake.store(true, Ordering::SeqCst);
                    });
                }
            }
        }
    });
    addr
}

struct Setup {
    pc: Pc,
    state: Arc<AppState>,
    url: String,
}

async fn setup(awake: bool, tweak: impl FnOnce(&mut Config)) -> Setup {
    let pc: Pc = Arc::new(FakePc::default());
    pc.awake.store(awake, Ordering::SeqCst);
    let strata = serve(
        Router::new()
            .route("/health", get(health))
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat))
            .with_state(pc.clone()),
    )
    .await;
    let agent = serve(
        Router::new()
            .route("/status", get(agent_status))
            .route("/sleep", post(agent_sleep))
            .with_state(pc.clone()),
    )
    .await;
    let wol = wol_listener(pc.clone()).await;
    let mut cfg = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        strata_url: format!("http://{strata}"),
        pc_mac: MAC,
        wol_addr: wol,
        pc_agent_url: Some(format!("http://{agent}")),
        pc_agent_token: None,
        idle_timeout: Duration::from_secs(3600),
        wake_timeout: Duration::from_secs(10),
        wol_resend: Duration::from_secs(1),
        health_poll: Duration::from_millis(50),
        ping_interval: Duration::from_millis(200),
        idle_check: Duration::from_millis(50),
        sleep_retry: Duration::from_millis(300),
        sleep_settle: Duration::from_millis(200),
        probe_timeout: Duration::from_millis(500),
        max_body_bytes: 1 << 20,
        data_dir: None,
    };
    tweak(&mut cfg);
    let state = AppState::new(cfg);
    state.spawn_idle_loop();
    let proxy = serve(api_router(state.clone())).await;
    Setup {
        pc,
        state,
        url: format!("http://{proxy}"),
    }
}

async fn post_json(url: &str, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

fn kinds(state: &AppState) -> Vec<EventKind> {
    state.events.recent().into_iter().map(|e| e.kind).collect()
}

#[tokio::test]
async fn awake_pc_is_passed_through() {
    let s = setup(true, |_| {}).await;
    let resp = post_json(&s.url, "/v1/chat/completions", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hallo");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sleeping_pc_is_woken_for_plain_request() {
    let s = setup(false, |_| {}).await;
    let resp = post_json(&s.url, "/v1/messages", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "Hallo");
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
    let kinds = kinds(&s.state);
    assert!(kinds.contains(&EventKind::WakeSent));
    assert!(kinds.contains(&EventKind::Awake));
}

#[tokio::test]
async fn sleeping_pc_streamed_request_gets_pings_then_answer() {
    let s = setup(false, |c| c.ping_interval = Duration::from_millis(100)).await;
    let resp = post_json(
        &s.url,
        "/v1/messages",
        json!({"model": "x", "stream": true}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.unwrap();
    assert!(text.starts_with("event: ping\n"), "{text}");
    let start = text.find("message_start").unwrap();
    assert!(text[..start].matches("event: ping").count() >= 2, "{text}");
    assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
}

#[tokio::test]
async fn openai_stream_uses_comment_pings_while_waking() {
    let s = setup(false, |c| c.ping_interval = Duration::from_millis(100)).await;
    let resp = post_json(&s.url, "/v1/chat/completions", json!({"stream": true})).await;
    let text = resp.text().await.unwrap();
    assert!(text.starts_with(": ping\n\n"), "{text}");
    assert!(!text.contains("event: ping"));
    assert!(text.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn gaming_mode_answers_at_once() {
    let s = setup(true, |_| {}).await;
    s.pc.gaming.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let resp = post_json(&s.url, "/v1/messages", json!({"model": "x"})).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()[STATUS_HEADER], "gaming");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 0);
    assert_eq!(s.state.pc_state(), PcState::Gaming);
}

#[tokio::test]
async fn wake_timeout_is_reported() {
    let s = setup(false, |c| c.wake_timeout = Duration::from_millis(800)).await;
    s.pc.ignore_wol.store(true, Ordering::SeqCst);
    let resp = post_json(&s.url, "/v1/chat/completions", json!({})).await;
    assert_eq!(resp.status(), 504);
    assert_eq!(resp.headers()[STATUS_HEADER], "wake_timeout");
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn idle_pc_is_sent_to_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(400)).await;
    post_json(&s.url, "/v1/messages", json!({})).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert!(!s.pc.up());
    assert_eq!(s.state.pc_state(), PcState::Asleep);

    // The next request wakes it again.
    let resp = post_json(&s.url, "/v1/messages", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(s.pc.wol_packets.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn running_stream_keeps_pc_awake() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(300)).await;
    let url = s.url.clone();
    let stream = tokio::spawn(async move {
        post_json(
            &url,
            "/v1/messages",
            json!({"stream": true, "pause_ms": 1200}),
        )
        .await
        .text()
        .await
        .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(s.state.in_flight(), 1);
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    let text = stream.await.unwrap();
    assert!(text.contains("message_stop"));
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pings_fill_long_pauses_between_events() {
    let s = setup(true, |c| c.ping_interval = Duration::from_millis(150)).await;
    let resp = post_json(
        &s.url,
        "/v1/messages",
        json!({"stream": true, "pause_ms": 700}),
    )
    .await;
    let text = resp.text().await.unwrap();
    let start = text.find("message_start").unwrap();
    let delta = text.find("content_block_delta").unwrap();
    assert!(
        text[start..delta].matches("event: ping").count() >= 2,
        "{text}"
    );
    // Every event stays whole: split on blank lines, each part is one event.
    for part in text.split("\n\n").filter(|p| !p.is_empty()) {
        assert!(part.starts_with("event: "), "{part:?}");
        assert_eq!(part.matches("data: ").count(), 1, "{part:?}");
    }
}

#[tokio::test]
async fn refused_sleep_is_retried_later() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(200)).await;
    s.pc.refuse_sleep.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
    assert!(kinds(&s.state).contains(&EventKind::SleepRefused));
    s.pc.refuse_sleep.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 2);
    assert!(!s.pc.up());
}

#[tokio::test]
async fn keep_awake_blocks_sleep() {
    let s = setup(true, |c| c.idle_timeout = Duration::from_millis(200)).await;
    s.state.keep_awake_for(Duration::from_millis(800));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 0);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(s.pc.sleep_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_requests_share_one_wake_up() {
    let s = setup(false, |c| c.wol_resend = Duration::from_secs(5)).await;
    let calls = (0..5).map(|_| post_json(&s.url, "/v1/messages", json!({})));
    for resp in futures_util::future::join_all(calls).await {
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(s.pc.wol_packets.load(Ordering::SeqCst), 1);
    assert_eq!(s.pc.api_requests.load(Ordering::SeqCst), 5);
}
