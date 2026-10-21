//! Shared test rig: a simulated PC with Strata, the PC service and a Wake on
//! LAN listener, all on localhost, plus a proxy in front of it.

#![allow(dead_code)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
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
pub use strata_pc_protocol::*;
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
    /// Strata answers API requests with 401 unless they carry this key. The
    /// PC service reports it with `key_generation`, as after a start of
    /// Strata; `set_strata_key` sets both.
    pub strata_key: Mutex<Option<String>>,
    pub key_generation: AtomicU64,
    /// The PC service is older than the key per start: no generation in its
    /// status and no `/strata/key`.
    pub no_key_endpoint: AtomicBool,
    /// `/strata/key` answers this instead of `strata_key`: a key Strata
    /// refuses.
    pub reported_key: Mutex<Option<String>>,
    pub key_requests: AtomicUsize,
    /// Strata's process runs but does not answer yet (starting).
    pub starting: AtomicBool,
    /// Strata answers, but its model is not in memory (`"loaded": false`).
    pub unloaded: AtomicBool,
    /// Bodies of the chat requests that were not streamed.
    pub chat_bodies: Mutex<Vec<Value>>,
    /// What a chat request without streaming answers instead of "Hallo".
    pub chat_answer: Mutex<Option<String>>,
    /// A chat request without streaming fails with 500.
    pub chat_fails: AtomicBool,
    /// Pause before a chat answer without streaming.
    pub chat_pause: Mutex<Duration>,
    /// Body of Strata's `/metrics?format=prometheus`, e.g. `METRICS_BUSY`; 404
    /// (an older build) if unset.
    pub metrics: Mutex<Option<String>>,
    pub metrics_requests: AtomicUsize,
    /// `Authorization` header of the last metrics request.
    pub metrics_auth: Mutex<Option<String>>,
    /// What the PC service's `GET /strata` answers; 404 (an older service) if unset.
    pub strata_info: Mutex<Option<StrataReport>>,
    /// The PC service's running or last Strata job.
    pub strata_job: Mutex<Option<StrataJob>>,
    /// Bodies of the jobs the PC service was asked for.
    pub job_bodies: Mutex<Vec<Value>>,
    /// The PC service refuses jobs with this reason.
    pub refuse_job: Mutex<Option<String>>,
    pub job_cancels: AtomicUsize,
    pub job_polls: AtomicUsize,
    /// Version the PC service reports in `/status`; none, like an old service, if unset.
    pub version: Mutex<Option<String>>,
    /// Hardware readings the PC service reports in `/status`; none, like a service
    /// from before them, if unset.
    pub hardware: Mutex<Option<Hardware>>,
    /// Calls of `POST /sleep/force`, answered or not.
    pub force_sleeps: AtomicUsize,
    /// Calls of `POST /strata/kill`, answered or not.
    pub strata_kills: AtomicUsize,
    /// An old PC service without the forced sleep and restart (404).
    pub no_force: AtomicBool,
    /// How long `POST /strata/kill` takes before it answers.
    pub kill_delay: Mutex<Duration>,
    pub update_requests: AtomicUsize,
    /// How long `POST /update` takes before it answers.
    pub update_delay: Mutex<Duration>,
    /// The PC service refuses updates with this reason (409).
    pub refuse_update: Mutex<Option<String>>,
    /// An old PC service without `/update` (404).
    pub no_update: AtomicBool,
    /// The version the PC service reports after an accepted update.
    pub update_to: Mutex<Option<String>>,
    /// What the PC service's `GET /strata-log` sends, in order; messages
    /// added later follow live.
    pub strata_log: Mutex<Vec<StrataOutputMessage>>,
    pub strata_log_requests: AtomicUsize,
    /// The PC service refuses `GET /strata-log` (wrong token, other address).
    pub refuse_log: AtomicBool,
    /// The PC service answers this many `/status` calls with 503, like a busy
    /// PC or network, although the PC stays up.
    pub status_misses: AtomicUsize,
    /// Power the simulated power plug reports, in watts.
    pub plug_watts: Mutex<f64>,
    /// The power plug answers like a Gen1 plug: `/status` only.
    pub plug_gen1: AtomicBool,
    /// The power plug answers every call with 503.
    pub plug_down: AtomicBool,
    /// The power plug takes longer than any probe timeout to answer.
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
            .is_some_and(|j| j.state == StrataJobState::Running)
    }
    /// Changes the job, e.g. its progress or its end.
    /// Strata starts with `key`, as the PC service does at every start.
    pub fn set_strata_key(&self, key: &str) {
        *self.strata_key.lock().unwrap() = Some(key.into());
        self.key_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The key generation in the status; none from an older service.
    fn reported_generation(&self) -> Option<u64> {
        (!self.no_key_endpoint.load(Ordering::SeqCst))
            .then(|| self.key_generation.load(Ordering::SeqCst))
    }

    pub fn update_job(&self, change: impl FnOnce(&mut StrataJob)) {
        change(self.strata_job.lock().unwrap().as_mut().expect("no job"));
    }
    /// What the PC service's `/status` answers now.
    fn status(&self) -> PcStatus {
        PcStatus {
            gaming_mode: self.gaming.load(Ordering::SeqCst),
            strata_running: self.strata_running(),
            strata_job: self.job_running(),
            mac: self.report_mac.lock().unwrap().clone(),
            version: self.version.lock().unwrap().clone(),
            hardware: self.hardware.lock().unwrap().clone(),
            strata_key_generation: self.reported_generation(),
            ..PcStatus::default()
        }
    }
}

pub type Pc = Arc<FakePc>;

/// Waits forever while the power plug hangs; 503 while it is down.
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
        Json(
            json!({"status": "ok", "loaded": loaded, "service": "strata",
                    "model": "Qwen3-30B-A3B", "max_context": 32768}),
        )
        .into_response()
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
    pc.chat_bodies.lock().unwrap().push(body);
    let pause = *pc.chat_pause.lock().unwrap();
    tokio::time::sleep(pause).await;
    if pc.chat_fails.load(Ordering::SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let content = pc
        .chat_answer
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "Hallo".into());
    Json(json!({"model": "Qwen3-30B-A3B", "choices": [{"message": {"content": content}}]}))
        .into_response()
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
    Json(pc.status()).into_response()
}

/// `GET /strata/key`: the key Strata takes; 404 without one.
pub async fn agent_strata_key(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.key_requests.fetch_add(1, Ordering::SeqCst);
    let key = pc.reported_key.lock().unwrap().clone();
    match (
        pc.reported_generation(),
        key.or_else(|| pc.strata_key.lock().unwrap().clone()),
    ) {
        (Some(generation), Some(api_key)) => Json(StrataKey {
            api_key,
            generation,
        })
        .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /update`: accepted, the service then reports `update_to`.
pub async fn agent_update(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.update_requests.fetch_add(1, Ordering::SeqCst);
    let delay = *pc.update_delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    if pc.no_update.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(reason) = pc.refuse_update.lock().unwrap().clone() {
        return (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response();
    }
    if let Some(version) = pc.update_to.lock().unwrap().clone() {
        *pc.version.lock().unwrap() = Some(version);
    }
    (StatusCode::ACCEPTED, Json(Answer::accepted())).into_response()
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

pub async fn agent_gaming(State(pc): State<Pc>, Json(body): Json<GamingRequest>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    pc.gaming.store(body.enabled, Ordering::SeqCst);
    Json(pc.status()).into_response()
}

pub async fn agent_sleep(State(pc): State<Pc>) -> Response {
    pc.sleep_requests.fetch_add(1, Ordering::SeqCst);
    if pc.refuse_sleep.load(Ordering::SeqCst) {
        return (StatusCode::CONFLICT, Json(Answer::refused("User active"))).into_response();
    }
    pc.awake.store(false, Ordering::SeqCst);
    Json(Answer::accepted()).into_response()
}

/// Cancels the running job, as a forced sleep or restart does; its kind.
fn cancel_for_force(pc: &FakePc) -> Option<StrataJobKind> {
    if !pc.job_running() {
        return None;
    }
    pc.update_job(|j| j.state = StrataJobState::Cancelled);
    pc.strata_job.lock().unwrap().as_ref().map(|j| j.kind)
}

/// `POST /sleep/force`: sleeps whatever holds it; a refused sleep counts
/// as user activity.
pub async fn agent_force_sleep(State(pc): State<Pc>) -> Response {
    pc.force_sleeps.fetch_add(1, Ordering::SeqCst);
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if pc.no_force.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let forced = ForcedSleep {
        user_active: pc.refuse_sleep.load(Ordering::SeqCst),
        strata_busy: false,
        gaming_mode: pc.gaming.load(Ordering::SeqCst),
        cancelled_job: cancel_for_force(&pc),
    };
    pc.awake.store(false, Ordering::SeqCst);
    Json(forced).into_response()
}

/// `POST /strata/kill`: refused while gaming, else Strata starts again and
/// loads.
pub async fn agent_strata_kill(State(pc): State<Pc>) -> Response {
    pc.strata_kills.fetch_add(1, Ordering::SeqCst);
    let delay = *pc.kill_delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if pc.no_force.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if pc.gaming.load(Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            Json(Refused::new("Gaming mode is on")),
        )
            .into_response();
    }
    let forced = ForcedRestart {
        cancelled_job: cancel_for_force(&pc),
        started: true,
    };
    pc.starting.store(true, Ordering::SeqCst);
    Json(forced).into_response()
}

/// A Strata the PC service reports: installed with one model, or not.
pub fn strata_info(installed: bool) -> StrataReport {
    let info = StrataInfo {
        installed,
        install: None,
        version: installed.then(|| "477ac13".into()),
        update_available: None,
        models: if installed {
            vec![model("IQ3_XXS", Some("qwen"), 81_000_000_000, true)]
        } else {
            vec![]
        },
        families: vec![family("qwen", "Qwen3.8-Flash-Next", true)],
        catalog: vec![StrataSize {
            id: "IQ3_XXS".into(),
            family: "qwen".into(),
            size: "IQ3_XXS".into(),
            download_gb: 75.8,
            ram_gb: 60,
            memory_with_vram: false,
            fits: true,
            partly_downloaded: false,
        }],
        recommended: Some("IQ3_XXS".into()),
        contexts: vec![32768, 65536],
        default_context: 65536,
        ram_gb: 64.0,
        vram_gb: None,
        disk_free_gb: Some(500.0),
        data_dir: "C:\\Strata-data".into(),
        machine: None,
        benchmark: None,
        benchmarks: vec![],
        job: None,
    };
    StrataReport { info, update: None }
}

/// An installed model as an older PC service reports it: without what
/// newer ones add. `id` is `<family>-<size>`, or the size for Qwen.
pub fn model(id: &str, family: Option<&str>, disk_bytes: u64, active: bool) -> StrataModel {
    StrataModel {
        id: id.into(),
        family: family.map(Into::into),
        size: family.map(|f| id.strip_prefix(&format!("{f}-")).unwrap_or(id).into()),
        disk_bytes,
        active,
        speed_projection: None,
        context: None,
        loop_recovery: None,
        sampling: None,
        config: None,
        chat_settings: None,
    }
}

pub fn family(id: &str, title: &str, speed_projection: bool) -> StrataFamily {
    StrataFamily {
        id: id.into(),
        title: title.into(),
        speed_projection,
    }
}

/// A job of `kind` without progress, output or result.
pub fn job(kind: StrataJobKind, state: StrataJobState) -> StrataJob {
    StrataJob {
        id: None,
        kind,
        model: None,
        state,
        step: None,
        percent: None,
        done_bytes: None,
        total_bytes: None,
        bytes_per_sec: None,
        eta_secs: None,
        lines: vec![],
        error: None,
        result: None,
        started: 0,
        ended: None,
    }
}

/// A benchmark run of `model` without the values newer services add.
pub fn benchmark(model: &str, result: &str) -> StrataBenchmark {
    StrataBenchmark {
        model: model.into(),
        result: result.into(),
        at: 1_760_000_000,
        at_text: String::new(),
        decode_tps: None,
        prompt_tps: None,
        context: None,
        speed_projection: None,
        version: None,
        error: None,
    }
}

pub async fn agent_strata(State(pc): State<Pc>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match pc.strata_info.lock().unwrap().clone() {
        Some(mut report) => {
            report.info.job = pc.strata_job.lock().unwrap().clone();
            Json(report).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Like the PC service: one job at a time, 202 with the job.
pub async fn agent_start_job(State(pc): State<Pc>, Json(body): Json<Value>) -> Response {
    if !pc.up() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let response = start_job(&pc, body.clone());
    // Noted last: a test that sees the request also sees the job it started.
    pc.job_bodies.lock().unwrap().push(body);
    response
}

fn start_job(pc: &Pc, body: Value) -> Response {
    // The PC service takes only what the protocol says.
    let request: StrataJobRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    };
    if let Some(reason) = pc.refuse_job.lock().unwrap().clone() {
        return (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response();
    }
    let model = match &request {
        StrataJobRequest::Install { size, .. } | StrataJobRequest::AddModel { size, .. } => {
            Some(size.clone())
        }
        StrataJobRequest::Activate { model }
        | StrataJobRequest::Delete { model }
        | StrataJobRequest::SetSpeedProjection { model, .. }
        | StrataJobRequest::SetContext { model, .. } => Some(model.clone()),
        _ => None,
    };
    let job = StrataJob {
        id: Some(1),
        model,
        ..job(request.kind(), StrataJobState::Running)
    };
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
            Json(Refused::new("No job is running")),
        )
            .into_response();
    }
    pc.job_cancels.fetch_add(1, Ordering::SeqCst);
    pc.update_job(|j| j.state = StrataJobState::Cancelled);
    StatusCode::ACCEPTED.into_response()
}

/// Like the PC service: the Strata output kept so far, then each new message.
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
                let msg = serde_json::to_string(&msg).unwrap();
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

/// A stand-in for the image's `strata-pc-agent.exe` that carries `version`
/// as the real program does, with the marker's prefix on its own before it
/// (the compiler may put other text right after such a copy).
pub fn agent_program(name: &str, version: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("strata-test-program-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("strata-pc-agent.exe");
    let mut bytes = b"MZ\x90\x00 code strata-pc-agent-version:called `Result::unwrap()`\n".to_vec();
    bytes.extend_from_slice(format!("strata-pc-agent-version:{version}\n").as_bytes());
    bytes.extend_from_slice(b"\x00 more code");
    std::fs::write(&exe, bytes).unwrap();
    exe
}

/// An empty `DATA_DIR` of its own for one test. A second `setup` over it
/// fails (its API key exists already); restart with `restarted`.
pub fn data_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("strata-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The proxy of `s` started again with the same config and `DATA_DIR`, in
/// front of the same simulated PC; `serve_proxy` puts it on the net.
pub fn restarted(s: &Setup) -> Arc<AppState> {
    AppState::new((*s.state.cfg()).clone())
}

/// The base URL of a proxy around `state`.
pub async fn serve_proxy(state: Arc<AppState>) -> String {
    format!("http://{}", serve(api_router(state)).await)
}

#[derive(Clone)]
pub struct Setup {
    pub pc: Pc,
    pub state: Arc<AppState>,
    pub url: String,
    /// An API key the proxy accepts; `post_json` sends it.
    pub key: String,
    /// Address of the simulated power plug, not set up by default.
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
            .route("/sleep/force", post(agent_force_sleep))
            .route("/strata/kill", post(agent_strata_kill))
            .route("/gaming", post(agent_gaming))
            .route("/strata", get(agent_strata))
            .route("/strata/jobs", post(agent_start_job))
            .route("/strata/job", get(agent_job))
            .route("/strata/job/cancel", post(agent_cancel_job))
            .route("/update", post(agent_update))
            .route("/strata-log", get(agent_strata_log))
            .route("/strata/key", get(agent_strata_key))
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
        pc_mac: Some(MAC),
        wol_addr: wol,
        pc_agent_url: Some(format!("http://{agent}")),
        pc_agent_token: None,
        shelly_url: None,
        code_bench_url: None,
        update_summary_timeout: None,
        code_bench_task_timeout: None,
        idle_timeout: Duration::from_secs(3600),
        wake_timeout: Duration::from_secs(10),
        wol_resend: Duration::from_secs(1),
        health_poll: Duration::from_millis(50),
        ping_interval: Duration::from_millis(200),
        idle_check: Duration::from_millis(50),
        plug_sample: Duration::from_millis(100),
        sleep_retry: Duration::from_millis(300),
        sleep_settle: Duration::from_millis(200),
        probe_timeout: Duration::from_millis(500),
        force_timeout: Duration::from_secs(10),
        max_body_bytes: 1 << 20,
        power_awake_watts: 75.0,
        power_asleep_watts: 3.0,
        price_ct_per_kwh: 30.0,
        data_dir: None,
        ui_password: None,
    };
    tweak(&mut cfg);
    let state = AppState::new(cfg);
    state.overview.set_poll(Duration::from_millis(100));
    let (key, _) = state.api_keys.create("Test").unwrap();
    state.spawn_idle_loop();
    state.spawn_plug_loop();
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
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

/// The events without the marks the proxy sets for the history on its
/// own, which can land at any time.
pub fn own_kinds(state: &AppState) -> Vec<EventKind> {
    kinds(state)
        .into_iter()
        .filter(|k| !k.is_history_mark())
        .collect()
}

/// Waits until the proxy has drawn this state of the PC in the history.
pub async fn seen(s: &Setup, kind: EventKind) {
    eventually(&format!("{kind:?} in the history"), || {
        kinds(&s.state).contains(&kind)
    })
    .await;
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
    /// what arrived so far, e.g. the shell with "Loading …" (#69).
    pub async fn page(&self, path: &str) -> String {
        use futures_util::StreamExt;
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
        if let Some(c) = &self.cookie {
            req = req.header("cookie", c);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut html = Vec::new();
        let Ok(resp) = tokio::time::timeout_at(deadline, req.send()).await else {
            panic!("{path}: no answer");
        };
        let mut body = resp
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .bytes_stream();
        loop {
            match tokio::time::timeout_at(deadline, body.next()).await {
                Ok(Some(chunk)) => html.extend_from_slice(&chunk.unwrap()),
                Ok(None) => return String::from_utf8(html).unwrap(),
                Err(_) => panic!(
                    "{path} does not finish, so far:\n{}",
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
        change(&mut b, "admin", "my own password").await.status(),
        200
    );
    b
}

/// A browser logged in with the password `logged_in` set before, e.g. after
/// a restart with the same `DATA_DIR`.
pub async fn logged_in_again(base: &str) -> Browser {
    let mut b = Browser::new(base);
    let resp = b.call("login", &[("password", "my own password")]).await;
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

    /// Opens another stream of the web UI, e.g. the Strata output.
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
                Err(_) => panic!("no message with {what}, last: {last}"),
            }
        }
    }
}
