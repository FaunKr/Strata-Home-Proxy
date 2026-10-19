//! Shared state: what the proxy knows about the PC and Strata, waking, and the
//! idle timer that sends the PC to sleep.

use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use strata_admin_ui::{
    api::PcView,
    labels::{NO_AGENT, NO_MAC, NOT_CONFIGURED},
};

use crate::{
    agent::{AgentClient, PcStatus, SleepAnswer},
    api_keys::ApiKeys,
    auth::Auth,
    clock,
    config::{Config, format_mac, parse_mac},
    events::{EventKind, History, Sample},
    overview::Monitor,
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

impl From<PcState> for PcView {
    fn from(pc: PcState) -> Self {
        match pc {
            PcState::Unknown => Self::Unknown,
            PcState::Asleep => Self::Asleep,
            PcState::Waking => Self::Waking,
            PcState::Awake => Self::Awake,
            PcState::Gaming => Self::Gaming,
        }
    }
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

/// Strata's answer on `/health`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Answers; `loaded` is false while the model is not in memory (Strata
    /// loads it with the next request).
    Up {
        loaded: bool,
    },
    Down,
}

#[derive(Deserialize)]
struct HealthBody {
    #[serde(default = "loaded_default")]
    loaded: bool,
}

fn loaded_default() -> bool {
    true
}

/// The PC service refused to sleep.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub reason: String,
    /// When it is asked again.
    pub retry_at: SystemTime,
}

struct Inner {
    pc: PcState,
    last_activity: Instant,
    /// `last_activity` as wall-clock time, for the countdown on the overview.
    last_activity_at: SystemTime,
    /// Start or end of the last forwarded request.
    last_request: Option<SystemTime>,
    last_healthy: Option<Instant>,
    sleep_accepted_at: Option<Instant>,
    next_sleep_attempt: Option<Instant>,
    /// The last refusal, until something happens.
    refused: Option<Refusal>,
    /// Wall-clock time, so it can be saved and survive a restart.
    keep_awake_until: Option<SystemTime>,
}

impl Inner {
    /// Starts the idle time again from now.
    fn touch(&mut self) {
        self.last_activity = Instant::now();
        self.last_activity_at = SystemTime::now();
        self.refused = None;
    }

    /// A forwarded request started or ended.
    fn note_request(&mut self) {
        self.touch();
        self.last_request = Some(SystemTime::now());
    }

    /// The PC is up. Woken or gaming just ended: the idle time starts now.
    /// Unknown before (just started): it keeps running from the start.
    fn mark_awake(&mut self) {
        match self.pc {
            PcState::Awake => {}
            PcState::Unknown => self.pc = PcState::Awake,
            _ => {
                self.pc = PcState::Awake;
                self.touch();
            }
        }
    }

    /// Notes the PC service's answer: gaming mode, a sleeping PC (no answer,
    /// unless Strata answers or a wake-up runs) or a PC that is up. True if
    /// the PC is up and not in gaming mode.
    fn note_pc_status(
        &mut self,
        status: Option<&PcStatus>,
        strata_answers: bool,
        settling: bool,
    ) -> bool {
        match status {
            None => {
                if !strata_answers && self.pc != PcState::Waking {
                    self.pc = PcState::Asleep;
                }
                false
            }
            Some(s) if s.gaming_mode => {
                self.pc = PcState::Gaming;
                false
            }
            // A PC that just accepted sleep may still answer.
            Some(_) if settling => false,
            // `ensure_awake` ends a wake-up once Strata answers.
            Some(_) if self.pc == PcState::Waking => true,
            Some(_) => {
                self.mark_awake();
                true
            }
        }
    }

    fn kept_awake(&self) -> bool {
        self.keep_awake_until.is_some_and(|t| t > SystemTime::now())
    }
}

/// The Ereignis of a state the proxy noticed on its own; `Waking` and
/// `Unknown` are steps on the way, and the history reads them from the
/// Ereignisse around them.
fn seen_event(pc: PcState) -> Option<EventKind> {
    match pc {
        PcState::Awake => Some(EventKind::PcSeenAwake),
        PcState::Asleep => Some(EventKind::PcSeenAsleep),
        PcState::Gaming => Some(EventKind::PcSeenGaming),
        PcState::Waking | PcState::Unknown => None,
    }
}

/// Writes to the shared state and records the PC's new state on the way out,
/// for the changes the proxy only noticed; used where the state comes from an
/// answer of the PC service or of Strata. A state the proxy brought about
/// itself keeps its own Ereignis instead.
struct ObservedWrite<'a> {
    state: &'a AppState,
    /// The state as it was before anything was written.
    from: PcState,
    inner: MutexGuard<'a, Inner>,
}

impl Deref for ObservedWrite<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl DerefMut for ObservedWrite<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

impl Drop for ObservedWrite<'_> {
    fn drop(&mut self) {
        let to = self.inner.pc;
        if to != self.from
            && let Some(kind) = seen_event(to)
        {
            self.state.events.record(kind, None);
        }
    }
}

/// What the overview needs from the shared state.
pub struct Snapshot {
    pub pc: PcState,
    pub in_flight: usize,
    pub last_activity_at: SystemTime,
    pub last_request: Option<SystemTime>,
    pub refused: Option<Refusal>,
    pub keep_awake_until: Option<SystemTime>,
}

#[derive(Serialize, Deserialize)]
struct KeepAwakeFile {
    /// Unix seconds.
    until: u64,
}

pub struct AppState {
    pub settings: Settings,
    /// Password and sessions of the web UI.
    pub auth: Auth,
    /// The clients' keys for the API.
    pub api_keys: ApiKeys,
    /// For forwarding; no overall timeout, since answers can take very long.
    pub upstream: Client,
    /// For `/health` and the PC service, rebuilt when `PROBE_TIMEOUT_MS` changes.
    probe: Mutex<(Duration, Client)>,
    pub events: History,
    /// Live state for the overview page.
    pub overview: Monitor,
    /// Strata jobs on the PC and the Strata key.
    pub strata: crate::strata::StrataState,
    /// Setting up the PC service from the NAS.
    pub pc_setup: crate::pc_setup::PcSetup,
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
        let keep_awake_until = keep_awake_file(&cfg.data_dir)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<KeepAwakeFile>(&text).ok())
            .map(|f| SystemTime::UNIX_EPOCH + Duration::from_secs(f.until))
            .filter(|t| *t > SystemTime::now());
        let state = Arc::new(Self {
            auth: Auth::new(cfg.data_dir.clone(), cfg.ui_password.clone()),
            api_keys: ApiKeys::new(cfg.data_dir.clone()),
            events: History::new(cfg.data_dir.clone()),
            upstream,
            overview: Monitor::default(),
            strata: crate::strata::StrataState::new(cfg.data_dir.clone()),
            pc_setup: crate::pc_setup::PcSetup::default(),
            probe: Mutex::new((cfg.probe_timeout, probe_client(cfg.probe_timeout))),
            in_flight: AtomicUsize::new(0),
            inner: Mutex::new(Inner {
                pc: PcState::Unknown,
                last_activity: Instant::now(),
                last_activity_at: SystemTime::now(),
                last_request: None,
                last_healthy: None,
                sleep_accepted_at: None,
                next_sleep_attempt: None,
                refused: None,
                keep_awake_until,
            }),
            wake_lock: tokio::sync::Mutex::new(()),
            settings,
        });
        // Before this start the proxy saw nothing, so the time until the first
        // known state of the history stays unknown.
        state.events.record(EventKind::ProxyStarted, None);
        state
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

    /// Client for `/health`, `/metrics` and the PC service.
    pub fn probe_client(&self) -> Client {
        self.probe(&self.cfg())
    }

    /// Asks the PC service for its status and takes over a reported MAC.
    pub async fn pc_status(&self, agent: &AgentClient) -> Option<PcStatus> {
        let status = agent.status().await?;
        if let Some(mac) = status.mac.as_deref().and_then(|m| parse_mac(m).ok())
            && self.settings.set_reported_mac(mac)
        {
            self.events
                .record(EventKind::MacLearned, Some(format_mac(mac)));
        }
        if self.pc_setup.note_version(status.version.as_deref()) {
            self.events
                .record(EventKind::PcServiceUpdated, status.version.clone());
        }
        crate::strata::note_status(self, agent, &status).await;
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
        self.set_keep_awake(Some(SystemTime::now() + duration));
    }

    /// Keeps the PC awake until `until`, or ends that with `None`; saved in
    /// `DATA_DIR` so a restart does not end it.
    pub fn set_keep_awake(&self, until: Option<SystemTime>) {
        self.inner.lock().unwrap().keep_awake_until = until;
        let Some(dir) = self.cfg().data_dir.clone() else {
            return;
        };
        let written = match until {
            Some(t) => {
                let file = KeepAwakeFile {
                    until: clock::unix(t),
                };
                let json = serde_json::to_string(&file).expect("serializes");
                crate::files::write_private(&dir, KEEP_AWAKE_FILE, json.as_bytes())
            }
            None => match std::fs::remove_file(dir.join(KEEP_AWAKE_FILE)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(e) = written {
            tracing::warn!("Wach halten nicht gespeichert: {e}");
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        Snapshot {
            pc: inner.pc,
            in_flight: self.in_flight(),
            last_activity_at: inner.last_activity_at,
            last_request: inner.last_request,
            refused: inner.refused.clone(),
            keep_awake_until: inner.keep_awake_until.filter(|t| *t > SystemTime::now()),
        }
    }

    fn set_pc(&self, pc: PcState) {
        self.inner.lock().unwrap().pc = pc;
    }

    /// The shared state, recording on the way out any state the proxy learned
    /// from an answer of the PC or of Strata.
    fn observed(&self) -> ObservedWrite<'_> {
        let inner = self.inner.lock().unwrap();
        ObservedWrite {
            from: inner.pc,
            state: self,
            inner,
        }
    }

    /// Takes over a state the PC service just reported.
    fn note_reported(&self, pc: PcState) {
        self.observed().inner.pc = pc;
    }

    /// Starts the idle time again from now, e.g. after waking by hand.
    pub fn touch(&self) {
        self.inner.lock().unwrap().touch();
    }

    /// True while a PC that just accepted a sleep request may still answer.
    fn settling(inner: &Inner, settle: Duration) -> bool {
        inner
            .sleep_accepted_at
            .is_some_and(|t| t.elapsed() < settle)
    }

    async fn strata_healthy(&self) -> bool {
        matches!(self.strata_health().await, Health::Up { .. })
    }

    /// Asks Strata's `/health`. A ready answer marks the PC awake, except
    /// while a PC that accepted sleep may still answer.
    pub async fn strata_health(&self) -> Health {
        let cfg = self.cfg();
        let Some(strata) = &cfg.strata_url else {
            return Health::Down;
        };
        let mut req = self.probe(&cfg).get(format!("{strata}/health"));
        if let Some(auth) = cfg.strata_authorization() {
            req = req.header(reqwest::header::AUTHORIZATION, auth);
        }
        let health = match req.send().await {
            Ok(r) if r.status().is_success() => Health::Up {
                // Strata says `{"status": "ok", "loaded": ...}`; other builds may not.
                loaded: r.json::<HealthBody>().await.map_or(true, |b| b.loaded),
            },
            _ => Health::Down,
        };
        let mut inner = self.observed();
        if Self::settling(&inner, cfg.sleep_settle) {
            return Health::Down;
        }
        if matches!(health, Health::Up { .. }) {
            inner.last_healthy = Some(Instant::now());
            inner.mark_awake();
        }
        health
    }

    /// Asks the PC service for its status for the overview, and notes gaming
    /// mode or a sleeping PC like the idle check does.
    pub async fn check_pc(&self, strata_answers: bool) -> Option<PcStatus> {
        let agent = self.agent();
        if !agent.configured() {
            return None;
        }
        let status = self.pc_status(&agent).await;
        let mut inner = self.observed();
        let settling = Self::settling(&inner, self.cfg().sleep_settle);
        inner.note_pc_status(status.as_ref(), strata_answers, settling);
        status
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
                self.note_reported(PcState::Gaming);
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
                        self.note_reported(PcState::Gaming);
                        return Err(WakeError::Gaming);
                    }
                    pc_answers = true;
                }
            }
            let settling = Self::settling(&self.inner.lock().unwrap(), cfg.sleep_settle);
            if !pc_answers && let Err(e) = self.send_wake_packet(&cfg, &mut last_wol).await {
                self.set_pc(PcState::Unknown);
                return Err(e);
            }
            if !settling && self.strata_healthy().await {
                let waited = started.elapsed();
                self.events.record(
                    EventKind::Awake,
                    Some(format!("after {:.1} s", waited.as_secs_f64())),
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

    /// Sends a magic packet unless the last one is younger than `WOL_RESEND_SECS`;
    /// the first one of a wake-up is recorded.
    async fn send_wake_packet(
        &self,
        cfg: &Config,
        last_wol: &mut Option<Instant>,
    ) -> Result<(), WakeError> {
        if last_wol.is_some_and(|t| t.elapsed() < cfg.wol_resend) {
            return Ok(());
        }
        let Some(mac) = cfg.pc_mac else {
            tracing::warn!("{NO_MAC}");
            return Err(WakeError::NoMac);
        };
        match wol::send(mac, cfg.wol_addr).await {
            Ok(()) if last_wol.is_none() => self.events.record(EventKind::WakeSent, None),
            Ok(()) => {}
            Err(e) => tracing::warn!("Weckpaket nicht gesendet: {e}"),
        }
        *last_wol = Some(Instant::now());
        Ok(())
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
        // A missed answer is no sleeping PC while Strata still answers (a
        // busy PC or network); waking it again would restart the idle time.
        let strata_answers = status.is_none() && self.strata_healthy().await;
        let now = Instant::now();
        {
            let mut inner = self.observed();
            let settling = Self::settling(&inner, cfg.sleep_settle);
            if !inner.note_pc_status(status.as_ref(), strata_answers, settling) {
                return;
            }
            if self.in_flight.load(Ordering::SeqCst) > 0
                || inner.last_activity.elapsed() < cfg.idle_timeout
                || inner.kept_awake()
                || inner.next_sleep_attempt.is_some_and(|t| t > now)
            {
                return;
            }
        }
        if let Err(e) = self.request_sleep(&agent).await {
            tracing::warn!("Schlafen-Anfrage fehlgeschlagen: {e}");
        }
    }

    /// Asks the PC service to sleep and notes the answer. A refusal or an
    /// error postpones the next automatic attempt by `SLEEP_RETRY_SECS`.
    pub async fn request_sleep(&self, agent: &AgentClient) -> anyhow::Result<SleepAnswer> {
        let retry = self.cfg().sleep_retry;
        let answer = agent.request_sleep().await;
        let mut inner = self.inner.lock().unwrap();
        match &answer {
            Ok(SleepAnswer::Accepted) => {
                inner.pc = PcState::Asleep;
                inner.sleep_accepted_at = Some(Instant::now());
                inner.last_healthy = None;
                inner.next_sleep_attempt = None;
                inner.refused = None;
                drop(inner);
                self.events.record(EventKind::SleepAccepted, None);
            }
            Ok(SleepAnswer::Refused(reason)) => {
                inner.next_sleep_attempt = Some(Instant::now() + retry);
                inner.refused = Some(Refusal {
                    reason: reason.clone(),
                    retry_at: SystemTime::now() + retry,
                });
                drop(inner);
                self.events
                    .record(EventKind::SleepRefused, Some(reason.clone()));
            }
            Err(_) => inner.next_sleep_attempt = Some(Instant::now() + retry),
        }
        answer
    }

    /// Sends magic packets until the PC service answers (not Strata), for
    /// switching gaming mode on a sleeping PC.
    /// Shares the wake lock with `ensure_awake`, so only one wake-up runs.
    pub async fn wake_until_agent(&self) -> Result<(), WakeError> {
        let _lock = self.wake_lock.lock().await;
        let started = Instant::now();
        let mut last_wol: Option<Instant> = None;
        loop {
            let cfg = self.cfg();
            // A PC that just accepted sleep may still answer: not awake yet.
            let settling = Self::settling(&self.inner.lock().unwrap(), cfg.sleep_settle);
            if !settling && self.pc_status(&self.agent()).await.is_some() {
                self.set_pc(PcState::Awake);
                if last_wol.is_some() {
                    self.events.record(
                        EventKind::Awake,
                        Some(format!("after {:.1} s", started.elapsed().as_secs_f64())),
                    );
                }
                return Ok(());
            }
            if last_wol.is_none() && cfg.pc_mac.is_some() {
                self.set_pc(PcState::Waking);
            }
            self.send_wake_packet(&cfg, &mut last_wol).await?;
            if started.elapsed() >= cfg.wake_timeout {
                self.set_pc(PcState::Unknown);
                self.events.record(EventKind::WakeTimeout, None);
                return Err(WakeError::Timeout);
            }
            tokio::time::sleep(cfg.health_poll).await;
        }
    }

    /// Switches gaming mode on the PC, waking it first if needed (recorded
    /// with `by`). Switching it off starts the idle time again, so the PC
    /// sleeps after the session.
    pub async fn set_gaming(&self, enabled: bool, by: &str) -> anyhow::Result<()> {
        let agent = self.agent();
        if !agent.configured() {
            anyhow::bail!(NO_AGENT);
        }
        if self.pc_status(&agent).await.is_none() {
            if !enabled {
                anyhow::bail!("The PC is asleep, so gaming mode is off already.");
            }
            let cfg = self.cfg();
            if cfg.strata_url.is_none() {
                anyhow::bail!(NOT_CONFIGURED);
            }
            if cfg.pc_mac.is_none() {
                anyhow::bail!(NO_MAC);
            }
            self.events.record(
                EventKind::WakeRequested,
                Some(format!("for gaming mode, {by}")),
            );
            self.wake_until_agent().await.map_err(|e| match e {
                WakeError::NoMac => anyhow::anyhow!(NO_MAC),
                _ => anyhow::anyhow!("The PC did not wake in time."),
            })?;
        }
        agent.set_gaming(enabled).await?;
        let mut inner = self.inner.lock().unwrap();
        if enabled {
            inner.pc = PcState::Gaming;
        } else {
            inner.pc = PcState::Awake;
            inner.touch();
        }
        Ok(())
    }

    pub fn spawn_idle_loop(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Read each round, so a changed IDLE_CHECK_SECS applies at once.
                tokio::time::sleep(state.cfg().idle_check).await;
                state.idle_tick().await;
                state.api_keys.flush_last_used();
            }
        });
    }

    /// Records the Leistung of the Messsteckdose every `PLUG_SAMPLE_SECS`
    /// while one is set up. A read that fails stores nothing; a hanging plug
    /// costs this loop one probe timeout and holds up nothing else.
    pub fn spawn_plug_loop(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Read each round, so a changed SHELLY_URL or PLUG_SAMPLE_SECS
                // applies at once.
                let started = Instant::now();
                let cfg = state.cfg();
                if let Some(url) = &cfg.shelly_url {
                    state.sample_plug(&cfg, url).await;
                }
                tokio::time::sleep(cfg.plug_sample.saturating_sub(started.elapsed())).await;
            }
        });
    }

    async fn sample_plug(&self, cfg: &Config, url: &str) {
        let ts = clock::unix_now();
        match crate::plug::power(&self.probe(cfg), url).await {
            Some(watts) => self.events.record_sample(Sample { ts, watts }),
            None => tracing::debug!("Steckdose {url} ohne Messwert"),
        }
    }
}

/// Counts a request as running from creation until drop. Moved into the
/// response body, so a long streamed answer keeps the PC awake to its end.
pub struct ActivityGuard(Arc<AppState>);

impl ActivityGuard {
    pub fn new(state: &Arc<AppState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::SeqCst);
        state.inner.lock().unwrap().note_request();
        Self(Arc::clone(state))
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.inner.lock().unwrap().note_request();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

const KEEP_AWAKE_FILE: &str = "keep-awake.json";

fn keep_awake_file(data_dir: &Option<PathBuf>) -> Option<PathBuf> {
    data_dir.as_ref().map(|dir| dir.join(KEEP_AWAKE_FILE))
}
