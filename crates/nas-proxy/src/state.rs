//! Shared state: what the proxy knows about the PC and Strata, waking, and the
//! idle timer that sends the PC to sleep.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use reqwest::Client;
use serde::Serialize;

use crate::{
    agent::{AgentClient, PcStatus, SleepAnswer},
    auth::Auth,
    config::{Config, format_mac, parse_mac},
    events::{EventKind, EventLog},
    settings::Settings,
    wol,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PcState {
    Unknown,
    Asleep,
    Waking,
    Awake,
    Gaming,
}

/// What a request should do right now.
#[derive(Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Gaming,
    NeedsWake,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WakeError {
    Gaming,
    Timeout,
    /// No MAC address is known, so no magic packet can be sent.
    NoMac,
}

struct Inner {
    pc: PcState,
    last_activity: Instant,
    last_healthy: Option<Instant>,
    sleep_accepted_at: Option<Instant>,
    next_sleep_attempt: Option<Instant>,
    keep_awake_until: Option<Instant>,
}

pub struct AppState {
    pub settings: Settings,
    /// Password and sessions of the web UI.
    pub auth: Auth,
    /// For forwarding; no overall timeout, since answers can take very long.
    pub upstream: Client,
    /// For `/health` and the PC service, rebuilt when `PROBE_TIMEOUT_MS` changes.
    probe: Mutex<(Duration, Client)>,
    pub events: EventLog,
    in_flight: AtomicUsize,
    inner: Mutex<Inner>,
    wake_lock: tokio::sync::Mutex<()>,
}

/// Healthy answers younger than this skip the `/health` check.
const HEALTH_CACHE: Duration = Duration::from_secs(5);
/// How often the PC service is asked for its status while waiting for a wake-up.
const AGENT_CHECK_WHILE_WAKING: Duration = Duration::from_secs(3);

fn probe_client(timeout: Duration) -> Client {
    Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .expect("HTTP client")
}

impl AppState {
    /// `cfg` holds defaults and environment; values saved on the settings page
    /// are loaded from its `data_dir` and override it.
    pub fn new(cfg: Config) -> Arc<Self> {
        Self::with_settings(Settings::new(cfg, &[]))
    }

    pub fn with_settings(settings: Settings) -> Arc<Self> {
        let cfg = settings.current();
        let upstream = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("HTTP client");
        Arc::new(Self {
            auth: Auth::new(cfg.data_dir.clone(), cfg.ui_password.clone()),
            events: EventLog::new(cfg.data_dir.clone()),
            upstream,
            probe: Mutex::new((cfg.probe_timeout, probe_client(cfg.probe_timeout))),
            in_flight: AtomicUsize::new(0),
            inner: Mutex::new(Inner {
                pc: PcState::Unknown,
                last_activity: Instant::now(),
                last_healthy: None,
                sleep_accepted_at: None,
                next_sleep_attempt: None,
                keep_awake_until: None,
            }),
            wake_lock: tokio::sync::Mutex::new(()),
            settings,
        })
    }

    /// The configuration in effect right now; changes on the settings page
    /// show up in the next call.
    pub fn cfg(&self) -> Arc<Config> {
        self.settings.current()
    }

    fn probe(&self, cfg: &Config) -> Client {
        let mut probe = self.probe.lock().unwrap();
        if probe.0 != cfg.probe_timeout {
            *probe = (cfg.probe_timeout, probe_client(cfg.probe_timeout));
        }
        probe.1.clone()
    }

    /// Asks the PC service for its status and takes over a reported MAC.
    async fn pc_status(&self, agent: &AgentClient) -> Option<PcStatus> {
        let status = agent.status().await?;
        if let Some(mac) = status.mac.as_deref().and_then(|m| parse_mac(m).ok())
            && self.settings.set_reported_mac(mac)
        {
            self.events
                .record(EventKind::MacLearned, Some(format_mac(mac)));
        }
        Some(status)
    }

    /// Client for the PC service with the current address and token.
    pub fn agent(&self) -> AgentClient {
        let cfg = self.cfg();
        AgentClient::new(&cfg, self.probe(&cfg))
    }

    pub fn pc_state(&self) -> PcState {
        self.inner.lock().unwrap().pc
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    pub fn keep_awake_for(&self, duration: Duration) {
        self.inner.lock().unwrap().keep_awake_until = Some(Instant::now() + duration);
    }

    fn set_pc(&self, pc: PcState) {
        self.inner.lock().unwrap().pc = pc;
    }

    /// True while a PC that just accepted a sleep request may still answer.
    fn settling(inner: &Inner, settle: Duration) -> bool {
        inner
            .sleep_accepted_at
            .is_some_and(|t| t.elapsed() < settle)
    }

    async fn strata_healthy(&self) -> bool {
        let cfg = self.cfg();
        let Some(strata) = &cfg.strata_url else {
            return false;
        };
        let url = format!("{strata}/health");
        let ok = matches!(self.probe(&cfg).get(url).send().await, Ok(r) if r.status().is_success());
        if ok {
            let mut inner = self.inner.lock().unwrap();
            if !Self::settling(&inner, cfg.sleep_settle) {
                inner.last_healthy = Some(Instant::now());
                if inner.pc != PcState::Awake {
                    inner.pc = PcState::Awake;
                    // Count idle time from the moment the PC is known to be up.
                    inner.last_activity = Instant::now();
                }
                return true;
            }
        }
        false
    }

    pub async fn readiness(&self) -> Readiness {
        {
            let inner = self.inner.lock().unwrap();
            if !Self::settling(&inner, self.cfg().sleep_settle)
                && inner
                    .last_healthy
                    .is_some_and(|t| t.elapsed() < HEALTH_CACHE)
            {
                return Readiness::Ready;
            }
        }
        if self.strata_healthy().await {
            return Readiness::Ready;
        }
        match self.pc_status(&self.agent()).await {
            Some(status) if status.gaming_mode => {
                self.set_pc(PcState::Gaming);
                Readiness::Gaming
            }
            _ => Readiness::NeedsWake,
        }
    }

    /// Wakes the PC if needed and waits until Strata answers on `/health`.
    /// Concurrent callers share one wake-up.
    pub async fn ensure_awake(&self) -> Result<(), WakeError> {
        let _lock = self.wake_lock.lock().await;
        if self.strata_healthy().await {
            return Ok(());
        }
        let started = Instant::now();
        let mut last_wol: Option<Instant> = None;
        let mut last_agent_check: Option<Instant> = None;
        let mut pc_answers = false;
        self.set_pc(PcState::Waking);
        loop {
            let cfg = self.cfg();
            // Ask the PC service now and then: gaming mode ends the wait, and an
            // answering service means the PC is up and only Strata is starting.
            if last_agent_check.is_none_or(|t| t.elapsed() >= AGENT_CHECK_WHILE_WAKING) {
                last_agent_check = Some(Instant::now());
                pc_answers = false;
                if let Some(status) = self.pc_status(&self.agent()).await {
                    if status.gaming_mode {
                        self.set_pc(PcState::Gaming);
                        return Err(WakeError::Gaming);
                    }
                    pc_answers = true;
                }
            }
            let settling = Self::settling(&self.inner.lock().unwrap(), cfg.sleep_settle);
            if !pc_answers && last_wol.is_none_or(|t| t.elapsed() >= cfg.wol_resend) {
                let Some(mac) = cfg.pc_mac else {
                    tracing::warn!("Keine MAC-Adresse bekannt, der PC kann nicht geweckt werden");
                    self.set_pc(PcState::Unknown);
                    return Err(WakeError::NoMac);
                };
                match wol::send(mac, cfg.wol_addr).await {
                    Ok(()) if last_wol.is_none() => {
                        self.events.record(EventKind::WakeSent, None);
                    }
                    Ok(()) => {}
                    Err(e) => tracing::warn!("Weckpaket nicht gesendet: {e}"),
                }
                last_wol = Some(Instant::now());
            }
            if !settling && self.strata_healthy().await {
                let waited = started.elapsed();
                self.events.record(
                    EventKind::Awake,
                    Some(format!("nach {:.1} s", waited.as_secs_f64())),
                );
                return Ok(());
            }
            if started.elapsed() >= cfg.wake_timeout {
                self.set_pc(PcState::Unknown);
                self.events.record(EventKind::WakeTimeout, None);
                return Err(WakeError::Timeout);
            }
            tokio::time::sleep(cfg.health_poll).await;
        }
    }

    /// One pass of the idle check: asks the PC to sleep when nothing has
    /// happened for `idle_timeout`.
    pub async fn idle_tick(&self) {
        let cfg = self.cfg();
        let agent = self.agent();
        if !agent.configured() {
            return;
        }
        let status = self.pc_status(&agent).await;
        let now = Instant::now();
        {
            let mut inner = self.inner.lock().unwrap();
            let settling = Self::settling(&inner, cfg.sleep_settle);
            match &status {
                None if inner.pc != PcState::Waking => {
                    inner.pc = PcState::Asleep;
                    return;
                }
                None => return,
                Some(s) if s.gaming_mode => {
                    inner.pc = PcState::Gaming;
                    return;
                }
                Some(_) if settling => return,
                Some(_) => {
                    if matches!(
                        inner.pc,
                        PcState::Asleep | PcState::Unknown | PcState::Gaming
                    ) {
                        // Woken by hand or gaming just ended: start counting now.
                        inner.pc = PcState::Awake;
                        inner.last_activity = now;
                    }
                }
            }
            if self.in_flight.load(Ordering::SeqCst) > 0
                || inner.last_activity.elapsed() < cfg.idle_timeout
                || inner.keep_awake_until.is_some_and(|t| t > now)
                || inner.next_sleep_attempt.is_some_and(|t| t > now)
            {
                return;
            }
        }
        match agent.request_sleep().await {
            Ok(SleepAnswer::Accepted) => {
                let mut inner = self.inner.lock().unwrap();
                inner.pc = PcState::Asleep;
                inner.sleep_accepted_at = Some(Instant::now());
                inner.last_healthy = None;
                inner.next_sleep_attempt = None;
                drop(inner);
                self.events.record(EventKind::SleepAccepted, None);
            }
            Ok(SleepAnswer::Refused(reason)) => {
                self.inner.lock().unwrap().next_sleep_attempt = Some(now + cfg.sleep_retry);
                self.events.record(EventKind::SleepRefused, Some(reason));
            }
            Err(e) => {
                tracing::warn!("Schlafen-Anfrage fehlgeschlagen: {e}");
                self.inner.lock().unwrap().next_sleep_attempt = Some(now + cfg.sleep_retry);
            }
        }
    }

    pub fn spawn_idle_loop(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Read each round, so a changed IDLE_CHECK_SECS applies at once.
                tokio::time::sleep(state.cfg().idle_check).await;
                state.idle_tick().await;
            }
        });
    }
}

/// Counts a request as running from creation until drop. Moved into the
/// response body, so a long streamed answer keeps the PC awake to its end.
pub struct ActivityGuard(Arc<AppState>);

impl ActivityGuard {
    pub fn new(state: &Arc<AppState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::SeqCst);
        state.inner.lock().unwrap().last_activity = Instant::now();
        Self(Arc::clone(state))
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().last_activity = Instant::now();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}
