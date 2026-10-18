//! The service: HTTP endpoints for the NAS proxy (see `docs/pc-agent-api.md`),
//! gaming mode and the Strata watchdog.

use std::{
    collections::HashMap,
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
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::IncomingStream,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    config::{Config, format_mac, parse_mac},
    managed::Managed,
    platform::Platform,
};

/// Delay between answering a sleep request and going to standby, so the
/// answer reaches the NAS first.
const SUSPEND_DELAY: Duration = Duration::from_secs(2);
/// A gap in wall-clock time this large between two watchdog passes means the
/// PC was asleep.
const RESUME_GAP: Duration = Duration::from_secs(30);
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
}

struct Watchdog {
    unhealthy_since: Option<Instant>,
    last_start: Option<Instant>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub gaming_mode: bool,
    pub strata_running: bool,
    pub user_idle_secs: u64,
    /// MAC address of the network card the caller reached, so the NAS can
    /// wake this PC without it being typed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GamingRequest {
    pub enabled: bool,
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
            .timeout(Duration::from_secs(3))
            .build()
            .expect("HTTP client");
        let gaming = gaming_flag(&cfg).exists();
        let managed = Managed::load(&cfg.state_dir());
        Arc::new(Self {
            cfg,
            platform,
            http,
            gaming: AtomicBool::new(gaming),
            watchdog: Mutex::new(Watchdog {
                unhealthy_since: None,
                last_start: None,
            }),
            macs: Mutex::new(HashMap::new()),
            managed: Mutex::new(managed),
        })
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

    async fn strata_healthy(&self) -> bool {
        matches!(self.strata_get("/health").send().await, Ok(r) if r.status().is_success())
    }

    /// True if Strata reports a slot at work. Strata builds without `/slots`
    /// count as idle; the NAS proxy also tracks open answers itself.
    async fn strata_busy(&self) -> bool {
        let Ok(resp) = self.strata_get("/slots").send().await else {
            return false;
        };
        match resp.json::<Vec<Slot>>().await {
            Ok(slots) => slots.iter().any(|s| s.is_processing),
            Err(_) => false,
        }
    }

    pub async fn status(&self) -> Status {
        Status {
            gaming_mode: self.gaming(),
            strata_running: self.strata_healthy().await,
            user_idle_secs: self.platform.user_idle().as_secs(),
            mac: None,
        }
    }

    /// The status for a caller that reached this PC at `local`, with the MAC
    /// address of that network card (or the one from the configuration).
    pub async fn status_for(self: &Arc<Self>, local: Option<IpAddr>) -> Status {
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
            return Err("Gaming-Modus ist an".into());
        }
        let idle = self.platform.user_idle();
        if idle < self.cfg.user_idle() {
            return Err(format!(
                "Jemand arbeitet am PC (letzte Eingabe vor {} s)",
                idle.as_secs()
            ));
        }
        if self.strata_busy().await {
            return Err("Strata bearbeitet eine Anfrage".into());
        }
        Ok(())
    }

    fn start_strata(&self) {
        self.watchdog.lock().unwrap().last_start = Some(Instant::now());
        tracing::info!("Strata wird gestartet");
        let managed = self.managed();
        let (program, args) = managed.start(&self.cfg);
        if let Err(e) = self.platform.start_strata(program, args) {
            tracing::error!("Strata-Start fehlgeschlagen: {e}");
        }
    }

    fn stop_strata(&self) {
        tracing::info!("Strata wird beendet");
        let managed = self.managed();
        if let Err(e) = self.platform.stop_strata(managed.processes(&self.cfg)) {
            tracing::error!("Strata-Ende fehlgeschlagen: {e}");
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
            tracing::warn!("Gaming-Schalter nicht gespeichert: {e}");
        }
        tracing::info!(enabled, "Gaming-Modus");
        if enabled {
            self.stop_strata();
        } else {
            self.start_strata();
        }
        self.watchdog.lock().unwrap().unhealthy_since = None;
    }

    /// One watchdog pass: starts Strata when it is missing and restarts it
    /// when it stopped answering, e.g. after standby upset the graphics driver.
    pub async fn watchdog_tick(&self) {
        if self.gaming() || !self.cfg.auto_start {
            return;
        }
        let healthy = self.strata_healthy().await;
        let now = Instant::now();
        let restart = {
            let mut wd = self.watchdog.lock().unwrap();
            if healthy {
                wd.unhealthy_since = None;
                return;
            }
            let loading = wd.last_start.is_some_and(|t| {
                now.duration_since(t) < Duration::from_secs(self.cfg.load_time_secs)
            });
            if loading {
                return;
            }
            let since = *wd.unhealthy_since.get_or_insert(now);
            now.duration_since(since) >= Duration::from_secs(self.cfg.restart_after_secs)
        };
        if restart {
            tracing::warn!("Strata antwortet nicht, Neustart");
            self.stop_strata();
            tokio::time::sleep(Duration::from_secs(2)).await;
            self.start_strata();
            self.watchdog.lock().unwrap().unhealthy_since = None;
        }
    }

    /// After standby, Strata gets a fresh chance: the unhealthy timer restarts
    /// so a short hiccup while the driver comes back is not counted.
    ///
    /// With `fresh_strata_after_standby`, Strata was ended before standby and
    /// is started again here, so the GPU begins with an empty memory.
    pub fn resumed(&self) {
        tracing::info!("PC ist aus dem Standby zurück");
        let just_started = {
            let mut wd = self.watchdog.lock().unwrap();
            wd.unhealthy_since = None;
            wd.last_start.is_some_and(|t| t.elapsed() < RESUME_GAP)
        };
        if self.cfg.fresh_strata_after_standby && !self.gaming() && !just_started {
            self.start_strata();
        }
    }

    /// Runs before standby; ends Strata when `fresh_strata_after_standby` is set.
    fn before_standby(&self) {
        if self.cfg.fresh_strata_after_standby {
            self.stop_strata();
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

async fn status(State(agent): State<Arc<Agent>>, req: Request) -> Json<Status> {
    let local = req
        .extensions()
        .get::<ConnectInfo<Peer>>()
        .and_then(|p| p.0.local)
        .map(|a| a.ip());
    Json(agent.status_for(local).await)
}

async fn sleep(State(agent): State<Arc<Agent>>) -> Response {
    if let Err(reason) = agent.may_sleep().await {
        tracing::info!(%reason, "Schlafen abgelehnt");
        return (
            StatusCode::CONFLICT,
            Json(json!({"accepted": false, "reason": reason})),
        )
            .into_response();
    }
    tracing::info!("Schlafen angenommen");
    let agent = Arc::clone(&agent);
    tokio::spawn(async move {
        tokio::time::sleep(SUSPEND_DELAY).await;
        agent.before_standby();
        let platform_agent = Arc::clone(&agent);
        let result = tokio::task::spawn_blocking(move || platform_agent.platform.suspend()).await;
        if let Err(e) = result.map_err(anyhow::Error::from).and_then(|r| r) {
            tracing::error!("Standby fehlgeschlagen: {e}");
        }
        // Also after a failed standby, so a stopped Strata comes back.
        agent.resumed();
    });
    Json(json!({"accepted": true})).into_response()
}

async fn gaming(State(agent): State<Arc<Agent>>, Json(req): Json<GamingRequest>) -> Json<Status> {
    agent.set_gaming(req.enabled);
    Json(agent.status().await)
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
        .route("/gaming", post(gaming))
        .layer(middleware::from_fn_with_state(agent.clone(), guard))
        .with_state(agent)
}
