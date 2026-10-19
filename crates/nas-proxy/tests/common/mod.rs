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
    http::{HeaderMap, StatusCode},
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
    /// `authorization` and `x-api-key` of the last API request Strata got.
    pub api_auth: Mutex<Option<SeenCredentials>>,
    /// The same for the last `/health` call.
    pub health_auth: Mutex<Option<SeenCredentials>>,
    /// Strata answers API requests with 401 unless they carry this key.
    pub strata_key: Mutex<Option<String>>,
    /// Strata's process runs but does not answer yet (starting).
    pub starting: AtomicBool,
    /// Strata answers, but its model is not in memory (`"loaded": false`).
    pub unloaded: AtomicBool,
    /// Body of Strata's `/metrics?format=prometheus`, e.g. `METRICS_BUSY`; 404
    /// (an older build) if unset.
    pub metrics: Mutex<Option<String>>,
    pub metrics_requests: AtomicUsize,
    /// `Authorization` header of the last metrics request.
    pub metrics_auth: Mutex<Option<String>>,
    /// What the PC service's `GET /strata` answers; 404 (an older service) if unset.
    pub strata_info: Mutex<Option<Value>>,
    /// The PC service's running or last Strata job.
    pub strata_job: Mutex<Option<Value>>,
    /// Bodies of the jobs the PC service was asked for.
    pub job_bodies: Mutex<Vec<Value>>,
    /// The PC service refuses jobs with this reason.
    pub refuse_job: Mutex<Option<String>>,
    pub job_cancels: AtomicUsize,
    pub job_polls: AtomicUsize,
    /// Version the PC service reports in `/status`; none, like an old service, if unset.
    pub version: Mutex<Option<String>>,
    /// Hardwarewerte the PC service reports in `/status`; none, like a service
    /// from before them, if unset.
    pub hardware: Mutex<Option<Value>>,
    pub update_requests: AtomicUsize,
    /// The PC service refuses updates with this reason (409).
    pub refuse_update: Mutex<Option<String>>,
    /// An old PC service without `/update` (404).
    pub no_update: AtomicBool,
    /// The version the PC service reports after an accepted update.
    pub update_to: Mutex<Option<String>>,
    /// What the PC service's `GET /strata-log` sends, in order; messages
    /// added later follow live.
    pub strata_log: Mutex<Vec<Value>>,
    pub strata_log_requests: AtomicUsize,
    /// The PC service refuses `GET /strata-log` (wrong token, other address).
    pub refuse_log: AtomicBool,
    /// The PC service answers this many `/status` calls with 503, like a busy
    /// PC or network, although the PC stays up.
    pub status_misses: AtomicUsize,
    /// Leistung the simulated Messsteckdose reports, in watts.
    pub plug_watts: Mutex<f64>,
    /// The Messsteckdose answers like a Gen1 plug: `/status` only.
    pub plug_gen1: AtomicBool,
    /// The Messsteckdose answers every call with 503.
    pub plug_down: AtomicBool,
    /// The Messsteckdose takes longer than any probe timeout to answer.
    pub plug_hangs: AtomicBool,
}

/// Credential headers as Strata saw them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeenCredentials {
    pub authorization: Option<String>,
    pub x_api_key: Option<String>,
}

impl SeenCredentials {
    fn of(headers: &HeaderMap) -> Self {
        let get = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_string());
        Self {
            authorization: get("authorization"),
            x_api_key: get("x-api-key"),
        }
    }
}

impl FakePc {
    pub fn up(&self) -> bool {
        self.awake.load(Ordering::SeqCst)
    }
    /// Strata's process runs (gaming mode ends it).
    pub fn strata_running(&self) -> bool {
        self.up() && !self.gaming.load(Ordering::SeqCst)
    }
    /// Strata answers.
    pub fn strata_up(&self) -> bool {
        self.strata_running() && !self.starting.load(Ordering::SeqCst)
    }
    pub fn job_running(&self) -> bool {
        self.strata_job
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|j| j["state"] == "running")
    }
    /// Sets fields of the job, e.g. its progress or its end.
    pub fn update_job(&self, fields: Value) {
        let mut job = self.strata_job.lock().unwrap();
        let job = job.as_mut().expect("kein Vorgang");
        for (k, v) in fields.as_object().unwrap() {
            job[k] = v.clone();
        }
    }
}

pub type Pc = Arc<FakePc>;

/// Waits forever while the Messsteckdose hangs; 503 while it is down.
async fn plug_trouble(pc: &FakePc) -> Option<Response> {
    if pc.plug_hangs.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    pc.plug_down
        .load(Ordering::SeqCst)
        .then(|| StatusCode::SERVICE_UNAVAILABLE.into_response())
}

/// Like a Gen2+ Shelly plug's `GET /rpc/Switch.GetStatus?id=0`; 404 on Gen1.
pub async fn plug_switch_status(State(pc): State<Pc>) -> Response {
    if let Some(trouble) = plug_trouble(&pc).await {
        return trouble;
    }
    if pc.plug_gen1.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let watts = *pc.plug_watts.lock().unwrap();
    Json(json!({"id": 0, "source": "init", "output": true, "apower": watts, "voltage": 231.2}))
        .into_response()
}

/// Like a Gen1 Shelly plug's `GET /status`; Gen2+ plugs have none.
pub async fn plug_status(State(pc): State<Pc>) -> Response {
    if let Some(trouble) = plug_trouble(&pc).await {
        return trouble;
    }
    if !pc.plug_gen1.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let watts = *pc.plug_watts.lock().unwrap();
    Json(json!({"relays": [{"ison": true}], "meters": [{"power": watts, "is_valid": true}]}))
        .into_response()
}

/// Like Strata: 200 with `loaded`; a stand-in 503 when it would not answer.
pub async fn health(State(pc): State<Pc>, headers: HeaderMap) -> Response {
    *pc.health_auth.lock().unwrap() = Some(SeenCredentials::of(&headers));
    if pc.strata_up() {
        let loaded = !pc.unloaded.load(Ordering::SeqCst);
        Json(json!({"status": "ok", "loaded": loaded, "service": "strata"})).into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
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

pub async fn messages(
    State(pc): State<Pc>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !pc.strata_up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    *pc.api_auth.lock().unwrap() = Some(SeenCredentials::of(&headers));
    if let Some(key) = pc.strata_key.lock().unwrap().clone()
        && SeenCredentials::of(&headers).authorization != Some(format!("Bearer {key}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
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

pub async fn chat(State(pc): State<Pc>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !pc.strata_up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    *pc.api_auth.lock().unwrap() = Some(SeenCredentials::of(&headers));
    if let Some(key) = pc.strata_key.lock().unwrap().clone()
        && SeenCredentials::of(&headers).authorization != Some(format!("Bearer {key}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
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
    let misses = pc.status_misses.load(Ordering::SeqCst);
    let missed = misses > 0
        && pc
            .status_misses
            .compare_exchange(misses, misses - 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
    if missed || !pc.up() {
        // A sleeping PC does not answer; a refused status is the closest stand-in.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut status = json!({
        "gaming_mode": pc.gaming.load(Ordering::SeqCst),
        "strata_running": pc.strata_running(),
        "strata_job": pc.job_running(),
    });
    if let Some(mac) = pc.report_mac.lock().unwrap().clone() {
        status["mac"] = json!(mac);
    }
    if let Some(version) = pc.version.lock().unwrap().clone() {
        status["version"] = json!(version);
    }
    if let Some(hardware) = pc.hardware.lock().unwrap().clone() {
        status["hardware"] = hardware;
    }
    Json(status).into_response()
}

/// `POST /update`: accepted, the service then reports `update_to`.
pub async fn agent_update(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.update_requests.fetch_add(1, Ordering::SeqCst);
    if pc.no_update.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(reason) = pc.refuse_update.lock().unwrap().clone() {
        return (StatusCode::CONFLICT, Json(json!({"reason": reason}))).into_response();
    }
    if let Some(version) = pc.update_to.lock().unwrap().clone() {
        *pc.version.lock().unwrap() = Some(version);
    }
    (StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response()
}

/// Strata's `/metrics`: the text set in `metrics` when asked for Prometheus'
/// format, like Strata.
pub async fn metrics(
    State(pc): State<Pc>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: axum::http::HeaderMap,
) -> Response {
    if !pc.strata_up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !query.unwrap_or_default().contains("format=prometheus") {
        return Json(json!({})).into_response();
    }
    pc.metrics_requests.fetch_add(1, Ordering::SeqCst);
    *pc.metrics_auth.lock().unwrap() = headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_string());
    match pc.metrics.lock().unwrap().clone() {
        Some(body) => body.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Real output of Strata's `serve/prometheus.py` (docs/strata-api.md): one
/// request generating at 38.5 tokens/s with one waiting, and nothing running.
pub const METRICS_BUSY: &str = include_str!("../fixtures/strata-metrics-busy.prom");
pub const METRICS_IDLE: &str = include_str!("../fixtures/strata-metrics-idle.prom");

pub async fn agent_gaming(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.gaming
        .store(body["enabled"].as_bool().unwrap(), Ordering::SeqCst);
    Json(json!({"gaming_mode": pc.gaming.load(Ordering::SeqCst)})).into_response()
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

/// A Strata the PC service reports: installed with one model, or not.
pub fn strata_info(installed: bool) -> Value {
    let models = if installed {
        json!([{"id": "IQ3_XXS", "family": "qwen", "size": "IQ3_XXS",
                "disk_bytes": 81_000_000_000u64, "active": true}])
    } else {
        json!([])
    };
    json!({
        "installed": installed,
        "version": installed.then_some("477ac13"),
        "models": models,
        "families": [{"id": "qwen", "title": "Qwen3.8-Flash-Next", "speed_projection": true}],
        "catalog": [{"id": "IQ3_XXS", "family": "qwen", "size": "IQ3_XXS",
                     "download_gb": 75.8, "ram_gb": 60, "fits": true,
                     "partly_downloaded": false}],
        "recommended": "IQ3_XXS",
        "contexts": [32768, 65536],
        "default_context": 65536,
        "ram_gb": 64.0,
        "disk_free_gb": 500.0,
        "data_dir": "C:\\Strata-data",
        "job": null,
    })
}

pub async fn agent_strata(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match pc.strata_info.lock().unwrap().clone() {
        Some(mut info) => {
            info["job"] = pc.strata_job.lock().unwrap().clone().unwrap_or(Value::Null);
            Json(info).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Like the PC service: one job at a time, 202 with the job.
pub async fn agent_start_job(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.job_bodies.lock().unwrap().push(body.clone());
    if let Some(reason) = pc.refuse_job.lock().unwrap().clone() {
        return (StatusCode::CONFLICT, Json(json!({ "reason": reason }))).into_response();
    }
    let job = json!({
        "id": 1,
        "kind": body["kind"],
        "model": body.get("model").or(body.get("size")).cloned().unwrap_or(Value::Null),
        "state": "running",
        "lines": [],
    });
    *pc.strata_job.lock().unwrap() = Some(job.clone());
    (StatusCode::ACCEPTED, Json(job)).into_response()
}

pub async fn agent_job(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.job_polls.fetch_add(1, Ordering::SeqCst);
    Json(pc.strata_job.lock().unwrap().clone()).into_response()
}

pub async fn agent_cancel_job(State(pc): State<Pc>) -> Response {
    if !pc.job_running() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"reason": "Es läuft kein Vorgang"})),
        )
            .into_response();
    }
    pc.job_cancels.fetch_add(1, Ordering::SeqCst);
    pc.update_job(json!({"state": "cancelled"}));
    StatusCode::ACCEPTED.into_response()
}

/// Like the PC service: the Strata-Ausgabe kept so far, then each new message.
pub async fn agent_strata_log(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.strata_log_requests.fetch_add(1, Ordering::SeqCst);
    if pc.refuse_log.load(Ordering::SeqCst) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let s = stream::unfold((pc, 0usize), |(pc, sent)| async move {
        loop {
            // A PC that went to sleep drops the connection.
            if !pc.up() {
                return None;
            }
            let next = pc.strata_log.lock().unwrap().get(sent).cloned();
            if let Some(msg) = next {
                let event = Bytes::from(format!("data: {msg}\n\n"));
                return Some((Ok::<_, std::convert::Infallible>(event), (pc, sent + 1)));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(s))
        .unwrap()
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
            .route("/metrics", get(metrics))
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(chat))
            .with_state(pc),
    )
    .await
}

#[derive(Clone)]
pub struct Setup {
    pub pc: Pc,
    pub state: Arc<AppState>,
    pub url: String,
    /// An API key the proxy accepts; `post_json` sends it.
    pub key: String,
    /// Address of the simulated Messsteckdose, not set up by default.
    pub plug: String,
}

pub async fn setup(awake: bool, tweak: impl FnOnce(&mut Config)) -> Setup {
    let pc: Pc = Arc::new(FakePc::default());
    pc.awake.store(awake, Ordering::SeqCst);
    let strata = serve_strata(pc.clone()).await;
    let agent = serve(
        Router::new()
            .route("/status", get(agent_status))
            .route("/sleep", post(agent_sleep))
            .route("/gaming", post(agent_gaming))
            .route("/strata", get(agent_strata))
            .route("/strata/jobs", post(agent_start_job))
            .route("/strata/job", get(agent_job))
            .route("/strata/job/cancel", post(agent_cancel_job))
            .route("/update", post(agent_update))
            .route("/strata-log", get(agent_strata_log))
            .with_state(pc.clone()),
    )
    .await;
    let wol = wol_listener(pc.clone()).await;
    let plug = serve(
        Router::new()
            .route("/rpc/Switch.GetStatus", get(plug_switch_status))
            .route("/status", get(plug_status))
            .with_state(pc.clone()),
    )
    .await;
    let mut cfg = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        strata_url: Some(format!("http://{strata}")),
        strata_api_key: None,
        pc_mac: Some(MAC),
        wol_addr: wol,
        pc_agent_url: Some(format!("http://{agent}")),
        pc_agent_token: None,
        shelly_url: None,
        code_bench_url: None,
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
    state.overview.set_poll(Duration::from_millis(100));
    let (key, _) = state.api_keys.create("Test").unwrap();
    state.spawn_idle_loop();
    let proxy = serve(api_router(state.clone())).await;
    Setup {
        pc,
        state,
        url: format!("http://{proxy}"),
        key,
        plug: plug.to_string(),
    }
}

/// Waits up to 5 s until `check` holds; panics naming `what` otherwise.
pub async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..100 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} ist nicht eingetreten");
}

pub async fn post_json(s: &Setup, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}{path}", s.url))
        .bearer_auth(&s.key)
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

    /// Calls a server function that takes JSON, like the settings sections.
    pub async fn call_json(&mut self, name: &str, body: Value) -> reqwest::Response {
        let mut req = reqwest::Client::new()
            .post(format!("{}/admin/api/{name}", self.base))
            .json(&body);
        if let Some(c) = &self.cookie {
            req = req.header("cookie", c);
        }
        req.send().await.unwrap()
    }

    pub async fn session(&mut self) -> String {
        let resp = self.call("session", &[]).await;
        assert_eq!(resp.status(), 200);
        resp.json::<String>().await.unwrap()
    }

    /// The page's HTML. A page that does not finish fails the test with
    /// what arrived so far, e.g. the shell with "Lädt …" (#69).
    pub async fn page(&self, path: &str) -> String {
        use futures_util::StreamExt;
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
        if let Some(c) = &self.cookie {
            req = req.header("cookie", c);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut html = Vec::new();
        let Ok(resp) = tokio::time::timeout_at(deadline, req.send()).await else {
            panic!("{path}: keine Antwort");
        };
        let mut body = resp
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .bytes_stream();
        loop {
            match tokio::time::timeout_at(deadline, body.next()).await {
                Ok(Some(chunk)) => html.extend_from_slice(&chunk.unwrap()),
                Ok(None) => return String::from_utf8(html).unwrap(),
                Err(_) => panic!(
                    "{path} wird nicht fertig, bisher:\n{}",
                    String::from_utf8_lossy(&html)
                ),
            }
        }
    }
}

/// Saves several settings at once, as a section of the web UI does.
pub async fn save_settings(b: &mut Browser, values: &[(&str, &str)]) -> reqwest::Response {
    let values: Vec<Value> = values
        .iter()
        .map(|(key, value)| serde_json::json!({ "key": key, "value": value }))
        .collect();
    b.call_json("save-settings", serde_json::json!({ "values": values }))
        .await
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

/// A browser logged in with the password `logged_in` set before, e.g. after
/// a restart with the same `DATA_DIR`.
pub async fn logged_in_again(base: &str) -> Browser {
    let mut b = Browser::new(base);
    let resp = b
        .call("login", &[("password", "ein eigenes Passwort")])
        .await;
    assert_eq!(resp.status(), 200);
    b
}

/// The overview's live stream, read like a browser's EventSource does.
pub struct Live {
    body: std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: String,
}

impl Live {
    /// Opens the stream; panics unless it answers 200.
    pub async fn open(b: &Browser) -> Self {
        Self::open_at(b, "/admin/api/overview-stream").await
    }

    /// Opens another stream of the web UI, e.g. the Strata-Ausgabe.
    pub async fn open_at(b: &Browser, path: &str) -> Self {
        let resp = Self::request_at(b, path).await;
        assert_eq!(resp.status(), 200);
        Self {
            body: Box::pin(resp.bytes_stream()),
            buf: String::new(),
        }
    }

    pub async fn request(b: &Browser) -> reqwest::Response {
        Self::request_at(b, "/admin/api/overview-stream").await
    }

    pub async fn request_at(b: &Browser, path: &str) -> reqwest::Response {
        let mut req = reqwest::Client::new().get(format!("{}{path}", b.base));
        if let Some(c) = &b.cookie {
            req = req.header("cookie", c);
        }
        req.send().await.unwrap()
    }

    /// The next overview sent.
    pub async fn next(&mut self) -> Value {
        use futures_util::StreamExt;
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let event: String = self.buf.drain(..end + 2).collect();
                if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data:")) {
                    return serde_json::from_str(data.trim()).unwrap();
                }
                continue;
            }
            let chunk = self.body.next().await.expect("stream ended").unwrap();
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Waits up to 5 s for a message that matches; panics with the last one.
    pub async fn until(&mut self, what: &str, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut last = Value::Null;
        loop {
            match tokio::time::timeout_at(deadline, self.next()).await {
                Ok(v) if check(&v) => return v,
                Ok(v) => last = v,
                Err(_) => panic!("kein Stand mit {what}, zuletzt: {last}"),
            }
        }
    }
}
