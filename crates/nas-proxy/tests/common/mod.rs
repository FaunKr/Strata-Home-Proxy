//! Shared test rig: a simulated PC with Strata, the PC service and a Wake on
//! LAN listener, all on localhost, plus a proxy in front of it.

#![allow(dead_code)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
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
use strata_nas_proxy::{AppState, Config, api_router, events::EventKind, wol::magic_packet};
use tokio::net::{TcpListener, UdpSocket};

pub const MAC: [u8; 6] = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];

#[derive(Default)]
pub struct FakePc {
    pub awake: AtomicBool,
    pub gaming: AtomicBool,
    pub refuse_sleep: AtomicBool,
    pub ignore_wol: AtomicBool,
    pub wol_packets: AtomicUsize,
    pub sleep_requests: AtomicUsize,
    pub api_requests: AtomicUsize,
    /// MAC the PC wakes up for; `MAC` if unset.
    pub wake_mac: Mutex<Option<[u8; 6]>>,
    /// MAC in the last magic packet received.
    pub last_mac: Mutex<Option<[u8; 6]>>,
    /// MAC the fake PC service reports in `/status`.
    pub report_mac: Mutex<Option<String>>,
}

impl FakePc {
    pub fn up(&self) -> bool {
        self.awake.load(Ordering::SeqCst)
    }
    pub fn strata_up(&self) -> bool {
        self.up() && !self.gaming.load(Ordering::SeqCst)
    }
}

pub type Pc = Arc<FakePc>;

pub async fn health(State(pc): State<Pc>) -> StatusCode {
    if pc.strata_up() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// An event stream with a pause in the middle, like a model thinking.
pub fn sse(events: Vec<String>, pause_after_first: Duration) -> Response {
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

pub async fn messages(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
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

pub async fn chat(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
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

pub async fn agent_status(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        // A sleeping PC does not answer; a refused status is the closest stand-in.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut status = json!({
        "gaming_mode": pc.gaming.load(Ordering::SeqCst),
        "strata_running": pc.strata_up(),
    });
    if let Some(mac) = pc.report_mac.lock().unwrap().clone() {
        status["mac"] = json!(mac);
    }
    Json(status).into_response()
}

pub async fn agent_sleep(State(pc): State<Pc>) -> Response {
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

pub async fn serve(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router.into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Listens for magic packets and wakes the fake PC shortly after one arrives.
pub async fn wol_listener(pc: Pc) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 256];
        loop {
            let (n, _) = socket.recv_from(&mut buf).await.unwrap();
            if n != 102 {
                continue;
            }
            let mac: [u8; 6] = buf[6..12].try_into().unwrap();
            *pc.last_mac.lock().unwrap() = Some(mac);
            let wake_mac = pc.wake_mac.lock().unwrap().unwrap_or(MAC);
            if buf[..n] == magic_packet(wake_mac) {
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

/// A fake Strata for `pc`; returns its address.
pub async fn serve_strata(pc: Pc) -> SocketAddr {
    serve(
        Router::new()
            .route("/health", get(health))
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat))
            .with_state(pc),
    )
    .await
}

pub struct Setup {
    pub pc: Pc,
    pub state: Arc<AppState>,
    pub url: String,
}

pub async fn setup(awake: bool, tweak: impl FnOnce(&mut Config)) -> Setup {
    let pc: Pc = Arc::new(FakePc::default());
    pc.awake.store(awake, Ordering::SeqCst);
    let strata = serve_strata(pc.clone()).await;
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
        strata_url: Some(format!("http://{strata}")),
        pc_mac: Some(MAC),
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
        ui_password: None,
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

pub async fn post_json(url: &str, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

pub fn kinds(state: &AppState) -> Vec<EventKind> {
    state.events.recent().into_iter().map(|e| e.kind).collect()
}

/// A browser with one cookie jar.
pub struct Browser {
    pub base: String,
    pub cookie: Option<String>,
}

impl Browser {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.to_string(),
            cookie: None,
        }
    }

    /// Calls a server function with form fields; keeps the session cookie.
    pub async fn call(&mut self, name: &str, fields: &[(&str, &str)]) -> reqwest::Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut req = reqwest::Client::new()
            .post(format!("{}/admin/api/{name}", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body);
        if let Some(c) = &self.cookie {
            req = req.header("cookie", c);
        }
        let resp = req.send().await.unwrap();
        if let Some(set) = resp.headers().get("set-cookie") {
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_string();
            self.cookie = (!pair.ends_with('=')).then_some(pair);
        }
        resp
    }

    pub async fn session(&mut self) -> String {
        let resp = self.call("session", &[]).await;
        assert_eq!(resp.status(), 200);
        resp.json::<String>().await.unwrap()
    }

    pub async fn page(&self, path: &str) -> String {
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
        if let Some(c) = &self.cookie {
            req = req.header("cookie", c);
        }
        req.send().await.unwrap().text().await.unwrap()
    }
}

pub fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn change(b: &mut Browser, current: &str, new: &str) -> reqwest::Response {
    b.call(
        "change-password",
        &[("current", current), ("new", new), ("repeat", new)],
    )
    .await
}

/// A browser logged in with an own password.
pub async fn logged_in(base: &str) -> Browser {
    let mut b = Browser::new(base);
    b.call("login", &[("password", "admin")]).await;
    assert_eq!(
        change(&mut b, "admin", "ein eigenes Passwort")
            .await
            .status(),
        200
    );
    b
}
