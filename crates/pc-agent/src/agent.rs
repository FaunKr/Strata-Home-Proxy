//! The service: HTTP endpoints for the NAS proxy (see `docs/pc-agent-api.md`),
//! gaming mode and the Strata watchdog.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, Request, State, connect_info::Connected},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
    serve::IncomingStream,
};
use serde::Deserialize;
use strata_pc_protocol::{
    Answer, ForcedRestart, ForcedSleep, GamingRequest, PcStatus, Refused, StrataJobKind,
};
use tokio::sync::broadcast;

use crate::{
    config::{Config, format_mac, parse_mac},
    job::Jobs,
    managed::Managed,
    output::{StrataOutput, StrataOutputMessage, StrataOutputNotice},
    platform::Platform,
    strata::{self, Refusal, StrataJobRequest, UpdateCheck},
    strata_process::{StrataProcess, Times},
    update::UpdateError,
};

/// Delay between answering a sleep request and going to standby, so the
/// answer reaches the NAS first.
const SUSPEND_DELAY: Duration = Duration::from_secs(2);
/// A gap in wall-clock time this large between two watchdog passes means the
/// PC was asleep.
const RESUME_GAP: Duration = Duration::from_secs(30);
/// Strata runs on this PC: a connection that takes longer is none.
const STRATA_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a looked-up MAC address is reused before asking the system again.
const MAC_CACHE: Duration = Duration::from_secs(600);

/// A looked-up MAC address (or none found) and when it was looked up.
type CachedMac = (Option<[u8; 6]>, Instant);

/// Both ends of a connection: the caller, and the address of this PC it
/// reached, which names the network card the NAS sees.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub remote: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl Connected<IncomingStream<'_, tokio::net::TcpListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self {
            remote: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}

pub struct Agent {
    pub cfg: Config,
    platform: Box<dyn Platform>,
    http: reqwest::Client,
    gaming: AtomicBool,
    watchdog: Mutex<Watchdog>,
    macs: Mutex<HashMap<IpAddr, CachedMac>>,
    managed: Mutex<Managed>,
    pub(crate) jobs: Jobs,
    pub(crate) times: Times,
    pub(crate) updates: UpdateCheck,
    /// An update of this program is under way.
    pub(crate) updating: AtomicBool,
    /// A sleep request was taken and standby follows; a second one adds
    /// no second standby.
    standby_pending: AtomicBool,
    pub(crate) output: Arc<StrataOutput>,
}

struct Watchdog {
    unhealthy_since: Option<Instant>,
}

/// One entry of llama.cpp-style `/slots`; only the busy flag matters.
#[derive(Deserialize)]
struct Slot {
    #[serde(default)]
    is_processing: bool,
}

impl Agent {
    pub fn new(cfg: Config, platform: Box<dyn Platform>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            // Windows retries a refused connection to localhost for about
            // 2 s; with Strata stopped, `/status` then took longer than the
            // proxy waits, and the PC looked asleep.
            .connect_timeout(STRATA_CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(3))
            .build()
            .expect("HTTP client");
        let gaming = gaming_flag(&cfg).exists();
        let managed = Managed::load(&cfg.state_dir());
        if managed.start(&cfg).is_none() {
            tracing::info!("No start script set for Strata, the service does not start Strata");
        }
        let jobs = Jobs::load(&cfg.state_dir());
        let output = Arc::new(StrataOutput::new(cfg.state_dir()));
        Arc::new(Self {
            cfg,
            platform,
            http,
            gaming: AtomicBool::new(gaming),
            watchdog: Mutex::new(Watchdog {
                unhealthy_since: None,
            }),
            macs: Mutex::new(HashMap::new()),
            managed: Mutex::new(managed),
            jobs,
            times: Times::default(),
            updates: UpdateCheck::default(),
            updating: AtomicBool::new(false),
            standby_pending: AtomicBool::new(false),
            output,
        })
    }

    pub(crate) fn platform(&self) -> &dyn Platform {
        self.platform.as_ref()
    }

    /// Strata's process: every start and stop goes through it.
    pub fn strata(&self) -> StrataProcess<'_> {
        StrataProcess::of(self)
    }

    /// Replaces the managed values and keeps them for the next start.
    pub fn set_managed(&self, managed: Managed) -> anyhow::Result<()> {
        managed.save(&self.cfg.state_dir())?;
        *self.managed.lock().unwrap() = managed;
        Ok(())
    }

    pub fn managed(&self) -> Managed {
        self.managed.lock().unwrap().clone()
    }

    pub fn gaming(&self) -> bool {
        self.gaming.load(Ordering::SeqCst)
    }

    fn strata_get(&self, path: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(format!("{}{path}", self.cfg.strata_url));
        match self.managed().api_key(&self.cfg) {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    pub(crate) async fn strata_healthy(&self) -> bool {
        matches!(self.strata_get("/health").send().await, Ok(r) if r.status().is_success())
    }

    /// True if Strata reports a slot at work. Strata builds without `/slots`
    /// count as idle; the NAS proxy also tracks open answers itself.
    pub(crate) async fn strata_busy(&self) -> bool {
        let Ok(resp) = self.strata_get("/slots").send().await else {
            return false;
        };
        match resp.json::<Vec<Slot>>().await {
            Ok(slots) => slots.iter().any(|s| s.is_processing),
            Err(_) => false,
        }
    }

    pub async fn status(&self) -> PcStatus {
        PcStatus {
            gaming_mode: self.gaming(),
            // A Strata started moments ago runs, though it may not answer yet.
            strata_running: self.strata_healthy().await || self.strata().loading(),
            user_idle_secs: self.platform.user_idle().as_secs(),
            mac: None,
            strata_job: self.jobs.running(),
            version: Some(crate::version().to_string()),
            hardware: Some(self.platform.hardware()),
        }
    }

    /// The status for a caller that reached this PC at `local`, with the MAC
    /// address of that network card (or the one from the configuration).
    pub async fn status_for(self: &Arc<Self>, local: Option<IpAddr>) -> PcStatus {
        let mut status = self.status().await;
        status.mac = self.mac_for(local).await.map(format_mac);
        status
    }

    async fn mac_for(self: &Arc<Self>, local: Option<IpAddr>) -> Option<[u8; 6]> {
        if let Some(mac) = &self.cfg.mac {
            return parse_mac(mac);
        }
        let local = local.filter(|ip| !ip.is_unspecified())?;
        if let Some((mac, at)) = self.macs.lock().unwrap().get(&local)
            && at.elapsed() < MAC_CACHE
        {
            return *mac;
        }
        let agent = Arc::clone(self);
        let mac = tokio::task::spawn_blocking(move || agent.platform.mac_for(local))
            .await
            .ok()
            .flatten();
        self.macs
            .lock()
            .unwrap()
            .insert(local, (mac, Instant::now()));
        mac
    }

    /// Checks whether the PC may sleep; `Err` carries the reason it may not.
    pub async fn may_sleep(&self) -> Result<(), String> {
        if self.gaming() {
            return Err("Gaming mode is on".into());
        }
        if self.jobs.running() {
            return Err("A Strata job is running".into());
        }
        self.refuse_while_updating()?;
        let idle = self.platform.user_idle();
        if idle < self.cfg.user_idle() {
            return Err(format!(
                "Someone is using the PC (last input {} s ago)",
                idle.as_secs()
            ));
        }
        if self.strata_busy().await {
            return Err("Strata is processing a request".into());
        }
        Ok(())
    }

    /// Refuses while this program updates itself.
    fn refuse_while_updating(&self) -> Result<(), String> {
        if self.updating.load(Ordering::SeqCst) {
            return Err("The PC service is updating".into());
        }
        Ok(())
    }

    /// A forced sleep: only an update of this program refuses it. Notes
    /// what it overrides and cancels a running job; standby follows as
    /// after an accepted `/sleep`, and gaming mode stays as it is.
    pub async fn force_sleep(&self) -> Result<ForcedSleep, String> {
        self.refuse_while_updating()?;
        let forced = ForcedSleep {
            user_active: self.platform.user_idle() < self.cfg.user_idle(),
            strata_busy: self.strata_busy().await,
            gaming_mode: self.gaming(),
            cancelled_job: self.cancel_job(),
        };
        Ok(forced)
    }

    /// A forced Strata restart: cancels a running job and waits for its
    /// end, kills Strata (also one started by hand) and starts the managed
    /// one again where the service starts Strata at all. Refused in gaming
    /// mode and during an update, also one that begins while it waits.
    pub async fn force_restart(&self) -> Result<ForcedRestart, String> {
        if self.gaming() {
            return Err("Gaming mode is on".into());
        }
        self.refuse_while_updating()?;
        let cancelled_job = self.cancel_job();
        // A cancelled job rolls back and may restart Strata itself; the
        // kill comes after, so Strata always starts fresh.
        self.wait_for_cancelled_job().await;
        self.refuse_while_updating()?;
        tracing::warn!("Strata is restarted by force");
        let process = self.strata();
        process.stop();
        // An update beginning now hands over to the updated service, which
        // starts Strata then.
        let started = process
            .start_after_stop(|| {
                self.refuse_while_updating().map_err(|_| {
                    "The PC service began an update; Strata starts with the updated service"
                        .to_string()
                })
            })
            .await?;
        // Counted afresh; the load time after the start applies as usual.
        self.watchdog.lock().unwrap().unhealthy_since = None;
        Ok(ForcedRestart {
            cancelled_job,
            started,
        })
    }

    /// Waits until the job cancelled by a forced sleep or restart has
    /// ended, at most `cancel_wait_secs`; a job stuck beyond that is left
    /// running.
    async fn wait_for_cancelled_job(&self) {
        let limit = Instant::now() + self.cfg.cancel_wait();
        while self.jobs.running() {
            if Instant::now() >= limit {
                tracing::warn!("The cancelled Strata job does not end, carrying on without it");
                return;
            }
            tokio::time::sleep(crate::job::POLL).await;
        }
    }

    /// Cancels the running job, if one runs, and gives its kind; downloaded
    /// parts stay, so the same job continues later.
    fn cancel_job(&self) -> Option<StrataJobKind> {
        let kind = self.jobs.current().map(|j| j.kind);
        self.jobs.cancel().then_some(kind).flatten()
    }

    /// What the Strata output has to say besides its lines.
    async fn output_notice(&self) -> StrataOutputNotice {
        if self.gaming() {
            StrataOutputNotice::Gaming
        } else if self.output.reading() {
            StrataOutputNotice::Running
        } else if self.strata_healthy().await {
            StrataOutputNotice::NotStartedByAgent
        } else {
            StrataOutputNotice::NotRunning
        }
    }

    pub fn set_gaming(&self, enabled: bool) {
        if self.gaming.swap(enabled, Ordering::SeqCst) == enabled {
            return;
        }
        let flag = gaming_flag(&self.cfg);
        let written = if enabled {
            std::fs::write(&flag, b"an")
        } else {
            std::fs::remove_file(&flag).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
        };
        if let Err(e) = written {
            tracing::warn!("Gaming switch not saved: {e}");
        }
        tracing::info!(enabled, "Gaming mode");
        if enabled {
            // Downloaded parts stay; the same job continues there later.
            if self.jobs.cancel() {
                tracing::info!("Gaming mode on: Strata job cancelled");
            }
            self.strata().stop();
        } else {
            self.strata().start();
        }
        self.watchdog.lock().unwrap().unhealthy_since = None;
    }

    /// Whether the service starts Strata on its own now: auto-start on,
    /// gaming mode off, and no job, which sets Strata up or switches it, so
    /// starting it then would get in the way.
    pub(crate) fn may_start_strata_itself(&self) -> bool {
        self.cfg.auto_start && !self.gaming() && !self.jobs.running()
    }

    /// One watchdog pass: starts Strata when it is missing and restarts it
    /// when it stopped answering, e.g. after standby upset the graphics driver.
    pub async fn watchdog_tick(&self) {
        // An update stopped Strata for the updated service to start; checked
        // here too, so the watchdog neither waits on `/health` nor stops it.
        if !self.may_start_strata_itself() || self.updating.load(Ordering::SeqCst) {
            return;
        }
        // Nothing to start yet: the Strata page has not set a start script.
        if self.managed().start(&self.cfg).is_none() {
            return;
        }
        let healthy = self.strata_healthy().await;
        let loading = self.strata().loading();
        let now = Instant::now();
        let restart = {
            let mut wd = self.watchdog.lock().unwrap();
            if healthy {
                wd.unhealthy_since = None;
                return;
            }
            if loading {
                return;
            }
            let since = *wd.unhealthy_since.get_or_insert(now);
            now.duration_since(since) >= Duration::from_secs(self.cfg.restart_after_secs)
        };
        if restart {
            tracing::warn!("Strata does not answer, restarting");
            self.strata().restart_unless_job_runs().await;
            self.watchdog.lock().unwrap().unhealthy_since = None;
        }
    }

    /// After standby, Strata gets a fresh chance: the unhealthy timer restarts
    /// so a short hiccup while the driver comes back is not counted.
    ///
    /// With `fresh_strata_after_standby`, Strata was ended before standby and
    /// is started again here, so the GPU begins with an empty memory.
    pub fn resumed(&self) {
        tracing::info!("PC is back from standby");
        self.watchdog.lock().unwrap().unhealthy_since = None;
        let just_started = self.strata().started_within(RESUME_GAP);
        if self.cfg.fresh_strata_after_standby && !self.gaming() && !just_started {
            self.strata().start();
        }
    }

    /// Runs before standby; ends Strata when `fresh_strata_after_standby` is set.
    fn before_standby(&self) {
        if self.cfg.fresh_strata_after_standby {
            self.strata().stop();
        }
    }

    pub fn spawn_watchdog(self: &Arc<Self>, every: Duration) {
        let agent = Arc::clone(self);
        tokio::spawn(async move {
            let mut last_wall = SystemTime::now();
            loop {
                tokio::time::sleep(every).await;
                let wall = SystemTime::now();
                let gap = wall.duration_since(last_wall).unwrap_or_default();
                last_wall = wall;
                if gap > every + RESUME_GAP {
                    agent.resumed();
                }
                agent.watchdog_tick().await;
            }
        });
    }
}

fn gaming_flag(cfg: &Config) -> PathBuf {
    cfg.state_dir().join("gaming-mode")
}

async fn status(State(agent): State<Arc<Agent>>, req: Request) -> Json<PcStatus> {
    let local = req
        .extensions()
        .get::<ConnectInfo<Peer>>()
        .and_then(|p| p.0.local)
        .map(|a| a.ip());
    Json(agent.status_for(local).await)
}

async fn sleep(State(agent): State<Arc<Agent>>) -> Response {
    if let Err(reason) = agent.may_sleep().await {
        tracing::info!(%reason, "Sleep request refused");
        return (StatusCode::CONFLICT, Json(Answer::refused(reason))).into_response();
    }
    tracing::info!("Sleep request accepted");
    go_to_standby(&agent);
    Json(Answer::accepted()).into_response()
}

async fn force_sleep(State(agent): State<Arc<Agent>>) -> Response {
    match agent.force_sleep().await {
        Ok(forced) => {
            tracing::warn!(?forced, "Forced sleep");
            go_to_standby(&agent);
            Json(forced).into_response()
        }
        Err(reason) => (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response(),
    }
}

async fn force_restart(State(agent): State<Arc<Agent>>) -> Response {
    match agent.force_restart().await {
        Ok(forced) => Json(forced).into_response(),
        Err(reason) => (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response(),
    }
}

/// Standby after an accepted or forced sleep request: after a moment, so
/// the answer reaches the NAS first. A job cancelled by a forced sleep ends
/// before, or is left behind after `cancel_wait_secs`. One standby at a
/// time: a request while one is pending only joins it. An update begun in
/// the meantime calls it off.
fn go_to_standby(agent: &Arc<Agent>) {
    if agent.standby_pending.swap(true, Ordering::SeqCst) {
        tracing::info!("Standby already follows");
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        tokio::time::sleep(SUSPEND_DELAY).await;
        agent.wait_for_cancelled_job().await;
        if agent.updating.load(Ordering::SeqCst) {
            tracing::warn!("No standby: the PC service is updating");
            agent.standby_pending.store(false, Ordering::SeqCst);
            return;
        }
        agent.before_standby();
        let platform_agent = Arc::clone(&agent);
        let result = tokio::task::spawn_blocking(move || platform_agent.platform.suspend()).await;
        if let Err(e) = result.map_err(anyhow::Error::from).and_then(|r| r) {
            tracing::error!("Standby failed: {e}");
        }
        // Also after a failed standby, so a stopped Strata comes back.
        agent.resumed();
        agent.standby_pending.store(false, Ordering::SeqCst);
    });
}

async fn gaming(State(agent): State<Arc<Agent>>, Json(req): Json<GamingRequest>) -> Json<PcStatus> {
    agent.set_gaming(req.enabled);
    Json(agent.status().await)
}

async fn strata_view(State(agent): State<Arc<Agent>>) -> Json<strata::StrataReport> {
    strata::check_for_update(&agent);
    let view = tokio::task::spawn_blocking(move || strata::view(&agent))
        .await
        .expect("view task");
    Json(view)
}

async fn strata_job(State(agent): State<Arc<Agent>>) -> Json<Option<strata::StrataJob>> {
    Json(agent.jobs.current())
}

async fn start_job(
    State(agent): State<Arc<Agent>>,
    Json(request): Json<StrataJobRequest>,
) -> Response {
    match strata::start(&agent, request) {
        Ok(job) => (StatusCode::ACCEPTED, Json(job)).into_response(),
        Err(Refusal::BadRequest(reason)) => {
            (StatusCode::BAD_REQUEST, Json(Refused::new(reason))).into_response()
        }
        Err(Refusal::Conflict(reason)) => {
            (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response()
        }
    }
}

async fn cancel_job(State(agent): State<Arc<Agent>>) -> Response {
    if agent.jobs.cancel() {
        (StatusCode::ACCEPTED, Json(agent.jobs.current())).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(Refused::new("No job is running")),
        )
            .into_response()
    }
}

async fn update(State(agent): State<Arc<Agent>>) -> Response {
    match crate::update::update(&agent).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(Answer::accepted())).into_response(),
        Err(UpdateError::Refused(reason)) => {
            (StatusCode::CONFLICT, Json(Refused::new(reason))).into_response()
        }
        Err(UpdateError::Failed(reason)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Refused::new(reason)),
        )
            .into_response(),
    }
}

/// How often an open `GET /strata-log` checks whether its notice changed.
const NOTICE_EVERY: Duration = Duration::from_secs(1);

/// The kept Strata output, then each new line and every change of notice,
/// for as long as the caller stays.
async fn strata_log(State(agent): State<Arc<Agent>>) -> Response {
    struct Follow {
        agent: Arc<Agent>,
        rx: broadcast::Receiver<StrataOutputMessage>,
        queue: VecDeque<StrataOutputMessage>,
        notice: Option<StrataOutputNotice>,
        tick: tokio::time::Interval,
    }
    let (backlog, rx) = agent.output.subscribe();
    let follow = Follow {
        agent,
        rx,
        queue: backlog.into(),
        notice: None,
        tick: tokio::time::interval(NOTICE_EVERY),
    };
    let messages = futures_util::stream::unfold(follow, |mut f| async move {
        loop {
            if let Some(message) = f.queue.pop_front() {
                let event = Event::default()
                    .json_data(&message)
                    .expect("message serializes");
                return Some((Ok::<_, Infallible>(event), f));
            }
            tokio::select! {
                message = f.rx.recv() => match message {
                    Ok(message) => f.queue.push_back(message),
                    // Too slow to follow: the caller connects again and
                    // gets the kept lines anew.
                    Err(_) => return None,
                },
                _ = f.tick.tick() => {
                    let notice = f.agent.output_notice().await;
                    if f.notice != Some(notice) {
                        f.notice = Some(notice);
                        f.queue.push_back(StrataOutputMessage::Notice { reason: notice });
                    }
                }
            }
        }
    });
    Sse::new(messages)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Token check, plus the client list; localhost always passes the list.
async fn guard(State(agent): State<Arc<Agent>>, req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<Peer>>()
        .map(|c| c.0.remote.ip());
    let allowed = &agent.cfg.allowed_clients;
    let peer_ok =
        allowed.is_empty() || peer.is_some_and(|ip| ip.is_loopback() || allowed.contains(&ip));
    if !peer_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let expected = format!("Bearer {}", agent.cfg.token);
    let token_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == expected);
    if !token_ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(req).await
}

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/sleep", post(sleep))
        .route("/sleep/force", post(force_sleep))
        .route("/strata/kill", post(force_restart))
        .route("/gaming", post(gaming))
        .route("/strata", get(strata_view))
        .route("/strata/jobs", post(start_job))
        .route("/strata/job", get(strata_job))
        .route("/strata/job/cancel", post(cancel_job))
        .route("/update", post(update))
        .route("/strata-log", get(strata_log))
        .layer(middleware::from_fn_with_state(agent.clone(), guard))
        .with_state(agent)
}
