//! Shared state, and the shell around the PC lifecycle: asking the PC service
//! and Strata, waking, and the idle timer that sends the PC to sleep. What an
//! answer means for the PC's state is decided in `lifecycle`.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use strata_admin_ui::labels::{NO_AGENT, NO_MAC, NOT_CONFIGURED};
use strata_pc_protocol::PcStatus;

use crate::{
    agent::{AgentClient, SleepAnswer},
    api_keys::ApiKeys,
    auth::Auth,
    clock,
    config::{Config, format_mac, parse_mac},
    events::{EventKind, History, Sample},
    lifecycle::{Lifecycle, Now, Observed, Record, Timings},
    overview::Monitor,
    settings::Settings,
    strata_client::StrataClient,
    wol,
};

pub use crate::lifecycle::{PcState, Refusal, Snapshot};

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

/// Whether Strata answers on `/health`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Answers; `loaded` is false while the model is not in memory (Strata
    /// loads it with the next request).
    Up {
        loaded: bool,
    },
    Down,
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
    /// The code benchmark in the container code-bench.
    pub code_bench: crate::code_bench::CodeBench,
    in_flight: AtomicUsize,
    /// Requests sent to Strata up to this moment are given up: Strata has
    /// shown nothing since (`drop_lost_requests`).
    give_up: tokio::sync::watch::Sender<Option<Instant>>,
    /// Since when Strata shows nothing running while requests are in flight.
    quiet_since: Mutex<Option<Instant>>,
    /// `LOST_AFTER`; shorter in tests.
    lost_after: Mutex<Duration>,
    lifecycle: Mutex<Lifecycle>,
    wake_lock: tokio::sync::Mutex<()>,
}

/// How long Strata may show nothing running or waiting while requests are in
/// flight before those requests count as lost. Strata keeps a request it
/// works on in its metrics, prefill included; a lost one (an answer that
/// never comes, a connection left open) would otherwise keep the PC awake,
/// since the pings keep the client waiting too.
const LOST_AFTER: Duration = Duration::from_secs(300);

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
            pc_setup: crate::pc_setup::PcSetup::new(cfg.data_dir.clone()),
            code_bench: crate::code_bench::CodeBench::new(cfg.data_dir.clone()),
            probe: Mutex::new((cfg.probe_timeout, probe_client(cfg.probe_timeout))),
            in_flight: AtomicUsize::new(0),
            give_up: tokio::sync::watch::Sender::new(None),
            quiet_since: Mutex::new(None),
            lost_after: Mutex::new(LOST_AFTER),
            lifecycle: Mutex::new(Lifecycle::new(Now::read(), keep_awake_until)),
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

    /// Asks the PC service for its status and takes over what it tells
    /// besides the PC's state: a reported MAC, the PC service's version,
    /// and the end of a Strata job. Every caller wants these noted at once:
    /// a job's end carries the Strata key it handed over.
    pub async fn ask_pc(&self, agent: &AgentClient) -> Option<PcStatus> {
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

    /// Strata at the current address with the current key; `None` without
    /// an address.
    pub fn strata(&self) -> Option<StrataClient> {
        let cfg = self.cfg();
        StrataClient::new(&cfg, self.probe(&cfg), self.upstream.clone())
    }

    /// Client for the PC service with the current address and token.
    pub fn agent(&self) -> AgentClient {
        let cfg = self.cfg();
        AgentClient::new(&cfg, self.probe(&cfg))
    }

    pub fn pc_state(&self) -> PcState {
        self.lifecycle.lock().unwrap().pc()
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
        self.lifecycle.lock().unwrap().set_keep_awake(until);
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
            tracing::warn!("Keep awake not saved: {e}");
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.lifecycle
            .lock()
            .unwrap()
            .snapshot(self.in_flight(), Now::read())
    }

    /// The lifecycle with the time and durations for a call. Records go to
    /// [`Self::record`] while the guard is held, so two changes land in the
    /// history in the order they happened; `record` never takes this lock.
    fn lifecycle(&self) -> (MutexGuard<'_, Lifecycle>, Now, Timings) {
        let t = Timings::from(&*self.cfg());
        (self.lifecycle.lock().unwrap(), Now::read(), t)
    }

    fn record(&self, record: impl Into<Option<Record>>) {
        if let Some(Record { kind, detail }) = record.into() {
            self.events.record(kind, detail);
        }
    }

    /// Notes what a check of the PC learned and records it.
    fn note(&self, observe: impl FnOnce(&mut Lifecycle, Now, Timings) -> Observed) -> bool {
        let (mut lifecycle, now, t) = self.lifecycle();
        let observed = observe(&mut lifecycle, now, t);
        self.record(observed.record);
        observed.up
    }

    /// Starts the idle time again from now, e.g. after waking by hand.
    pub fn touch(&self) {
        self.lifecycle.lock().unwrap().touch(Now::read());
    }

    fn settling(&self) -> bool {
        let (lifecycle, now, t) = self.lifecycle();
        lifecycle.settling(now, t)
    }

    async fn strata_healthy(&self) -> bool {
        matches!(self.strata_health().await, Health::Up { .. })
    }

    /// Asks Strata's `/health`. A ready answer marks the PC awake, except
    /// while a PC that accepted sleep may still answer.
    pub async fn strata_health(&self) -> Health {
        let Some(strata) = self.strata() else {
            return Health::Down;
        };
        let health = match strata.health().await {
            Ok(h) => Health::Up { loaded: h.loaded },
            Err(_) => Health::Down,
        };
        let healthy = matches!(health, Health::Up { .. });
        if self.note(|l, now, t| l.strata_answered(healthy, now, t)) {
            health
        } else {
            Health::Down
        }
    }

    /// Asks the PC service for its status for the overview, and notes gaming
    /// mode or a sleeping PC like the idle check does.
    pub async fn check_pc(&self, strata_answers: bool) -> Option<PcStatus> {
        let agent = self.agent();
        if !agent.configured() {
            return None;
        }
        let status = self.ask_pc(&agent).await;
        self.note(|l, now, t| l.pc_answered(status.as_ref(), strata_answers, now, t));
        status
    }

    pub async fn readiness(&self) -> Readiness {
        {
            let (lifecycle, now, t) = self.lifecycle();
            if lifecycle.healthy_lately(now, t) {
                return Readiness::Ready;
            }
        }
        if self.strata_healthy().await {
            return Readiness::Ready;
        }
        match self.ask_pc(&self.agent()).await {
            Some(status) if status.gaming_mode => {
                self.seen_gaming();
                Readiness::Gaming
            }
            _ => Readiness::NeedsWake,
        }
    }

    /// The PC service reported gaming mode.
    fn seen_gaming(&self) {
        let (mut lifecycle, ..) = self.lifecycle();
        self.record(lifecycle.seen_gaming());
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
        self.lifecycle.lock().unwrap().wake_started();
        loop {
            let cfg = self.cfg();
            // Ask the PC service now and then: gaming mode ends the wait, and an
            // answering service means the PC is up and only Strata is starting.
            if last_agent_check.is_none_or(|t| t.elapsed() >= AGENT_CHECK_WHILE_WAKING) {
                last_agent_check = Some(Instant::now());
                pc_answers = false;
                if let Some(status) = self.ask_pc(&self.agent()).await {
                    if status.gaming_mode {
                        self.seen_gaming();
                        return Err(WakeError::Gaming);
                    }
                    pc_answers = true;
                }
            }
            let settling = self.settling();
            if !pc_answers && let Err(e) = self.send_wake_packet(&cfg, &mut last_wol).await {
                self.lifecycle.lock().unwrap().wake_failed();
                return Err(e);
            }
            if !settling && self.strata_healthy().await {
                let mut lifecycle = self.lifecycle.lock().unwrap();
                self.record(lifecycle.woke(started.elapsed(), Now::read()));
                return Ok(());
            }
            if started.elapsed() >= cfg.wake_timeout {
                let mut lifecycle = self.lifecycle.lock().unwrap();
                self.record(lifecycle.wake_timed_out());
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
            Err(e) => tracing::warn!("Wake packet not sent: {e}"),
        }
        *last_wol = Some(Instant::now());
        Ok(())
    }

    /// Gives up the requests in flight that were sent to Strata at least
    /// `LOST_AFTER` ago, once Strata has shown nothing running or waiting for
    /// that long; a request Strata has not had that long to pick up is left
    /// alone.
    pub async fn drop_lost_requests(&self) {
        if self.in_flight() == 0 {
            *self.quiet_since.lock().unwrap() = None;
            return;
        }
        let metrics = match self.strata() {
            Some(strata) => strata.metrics().await.ok(),
            None => None,
        };
        let quiet = metrics.is_some_and(|m| m.processing == 0 && m.deferred == 0);
        let mut quiet_since = self.quiet_since.lock().unwrap();
        if !quiet {
            *quiet_since = None;
            return;
        }
        let since = *quiet_since.get_or_insert_with(Instant::now);
        let Some(cutoff) = Instant::now().checked_sub(*self.lost_after.lock().unwrap()) else {
            return;
        };
        if since > cutoff {
            return;
        }
        let before = self.give_up.send_replace(Some(cutoff));
        if before.is_none_or(|b| b < since) {
            tracing::warn!("Open requests given up: Strata no longer shows them");
        }
    }

    /// Changes `LOST_AFTER`; for tests.
    pub fn set_lost_after(&self, after: Duration) {
        *self.lost_after.lock().unwrap() = after;
    }

    /// One pass of the idle check: asks the PC to sleep when nothing has
    /// happened for `idle_timeout`.
    pub async fn idle_tick(&self) {
        let agent = self.agent();
        if !agent.configured() {
            return;
        }
        let status = self.ask_pc(&agent).await;
        // A missed answer is no sleeping PC while Strata still answers (a
        // busy PC or network); waking it again would restart the idle time.
        let strata_answers = status.is_none() && self.strata_healthy().await;
        {
            let (mut lifecycle, now, t) = self.lifecycle();
            let seen = lifecycle.pc_answered(status.as_ref(), strata_answers, now, t);
            self.record(seen.record);
            if !seen.up || !lifecycle.wants_sleep(self.in_flight(), now, t) {
                return;
            }
        }
        if let Err(e) = self.request_sleep(&agent).await {
            tracing::warn!("Sleep request failed: {e}");
        }
    }

    /// Asks the PC service to sleep and notes the answer. A refusal or an
    /// error postpones the next automatic attempt by `SLEEP_RETRY_SECS`.
    pub async fn request_sleep(&self, agent: &AgentClient) -> anyhow::Result<SleepAnswer> {
        let answer = agent.request_sleep().await;
        let (mut lifecycle, now, t) = self.lifecycle();
        self.record(lifecycle.sleep_answered(answer.as_ref().ok(), now, t));
        answer
    }

    /// Notes a forced sleep the PC service took, with `detail`; from then
    /// on as after an accepted sleep request.
    pub fn forced_sleep_accepted(&self, detail: String) {
        let (mut lifecycle, now, t) = self.lifecycle();
        self.events.record(EventKind::ForcedSleep, Some(detail));
        self.record(lifecycle.sleep_answered(Some(&SleepAnswer::Accepted), now, t));
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
            if !self.settling() && self.ask_pc(&self.agent()).await.is_some() {
                let mut lifecycle = self.lifecycle.lock().unwrap();
                if last_wol.is_some() {
                    self.record(lifecycle.woke(started.elapsed(), Now::read()));
                } else {
                    // Woken by someone else, e.g. at the desk.
                    self.record(lifecycle.woken_elsewhere());
                }
                return Ok(());
            }
            if last_wol.is_none() && cfg.pc_mac.is_some() {
                self.lifecycle.lock().unwrap().wake_started();
            }
            self.send_wake_packet(&cfg, &mut last_wol).await?;
            if started.elapsed() >= cfg.wake_timeout {
                let mut lifecycle = self.lifecycle.lock().unwrap();
                self.record(lifecycle.wake_timed_out());
                return Err(WakeError::Timeout);
            }
            tokio::time::sleep(cfg.health_poll).await;
        }
    }

    /// Switches gaming mode on the PC, waking it first if needed, and records
    /// it with `by`. Switching it off starts the idle time again, so the PC
    /// sleeps after the session.
    pub async fn set_gaming(&self, enabled: bool, by: &str) -> anyhow::Result<()> {
        let agent = self.agent();
        if !agent.configured() {
            anyhow::bail!(NO_AGENT);
        }
        if self.ask_pc(&agent).await.is_none() {
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
        let mut lifecycle = self.lifecycle.lock().unwrap();
        self.record(lifecycle.gaming_switched(enabled, by, Now::read()));
        Ok(())
    }

    /// Notes every `events::BEAT` that the proxy runs, so the history knows
    /// after a stop since when it saw nothing.
    pub fn spawn_beat_loop(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                state.events.beat();
                tokio::time::sleep(crate::events::BEAT).await;
            }
        });
    }

    pub fn spawn_idle_loop(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Read each round, so a changed IDLE_CHECK_SECS applies at once.
                tokio::time::sleep(state.cfg().idle_check).await;
                state.drop_lost_requests().await;
                state.idle_tick().await;
                state.api_keys.flush_last_used();
            }
        });
    }

    /// Records the power of the power plug every `PLUG_SAMPLE_SECS`
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
            None => tracing::debug!("Power plug {url} gave no reading"),
        }
    }
}

/// Counts a request as running from creation until drop. Moved into the
/// response body, so a long streamed answer keeps the PC awake to its end.
pub struct ActivityGuard(Arc<AppState>);

impl ActivityGuard {
    pub fn new(state: &Arc<AppState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::SeqCst);
        state.lifecycle.lock().unwrap().request_seen(Now::read());
        Self(Arc::clone(state))
    }

    /// Resolves once the request, sent to Strata at `sent`, is given up as lost.
    pub fn given_up(&self, sent: Instant) -> impl Future<Output = ()> + Send + use<> {
        let mut rx = self.0.give_up.subscribe();
        async move {
            let closed = rx
                .wait_for(|cutoff| cutoff.is_some_and(|c| sent <= c))
                .await
                .is_err();
            if closed {
                // The state is gone, so nothing gives the request up anymore.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.lifecycle.lock().unwrap().request_seen(Now::read());
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

const KEEP_AWAKE_FILE: &str = "keep-awake.json";

fn keep_awake_file(data_dir: &Option<PathBuf>) -> Option<PathBuf> {
    data_dir.as_ref().map(|dir| dir.join(KEEP_AWAKE_FILE))
}
