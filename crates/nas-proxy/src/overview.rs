//! The live overview: while at least one page watches, the proxy asks the PC
//! service, Strata's `/health` and its metrics every few seconds and
//! sends the overview again whenever it changes. Without watchers it asks
//! nothing.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use futures_util::{StreamExt, stream::BoxStream};
use strata_admin_ui::api::{
    EventView, HardwareView, HistoryView, Overview, PcView, PowerView, Setup, SleepView, StrataJob,
    StrataView,
};
use strata_pc_protocol::PcStatus;
use tokio::sync::{Notify, watch};

use crate::{
    clock,
    state::{AppState, Health},
    strata_client::Metrics,
};

/// How often the PC and Strata are asked while a page watches.
const POLL: Duration = Duration::from_secs(5);
/// The same while a Strata job runs, so its progress moves along.
const POLL_JOB: Duration = Duration::from_secs(2);
/// How often the overview is rebuilt from what the proxy knows, without asking
/// anyone; changes from requests and buttons show up this fast.
const REFRESH: Duration = Duration::from_millis(250);
const EVENTS_SHOWN: usize = 10;
/// How often the history of the last 7 days is summed again for the overview;
/// it changes by the hour, not by the tick.
const WEEK_REFRESH: Duration = Duration::from_secs(60);

/// The answers of the last round of asking.
#[derive(Clone, Debug)]
struct Probe {
    health: Health,
    metrics: Option<Metrics>,
    pc: Option<PcStatus>,
    /// The running Strata job, asked for only while one runs.
    job: Option<StrataJob>,
    /// The power of the power plug.
    power: PowerView,
}

pub struct Monitor {
    tx: watch::Sender<Option<Overview>>,
    watchers: AtomicUsize,
    running: AtomicBool,
    probe: Mutex<Option<Probe>>,
    probe_now: Notify,
    poll: Mutex<Duration>,
    /// Rounds of asking so far; for tests.
    rounds: AtomicUsize,
    /// The last 7 days of the history and when they were summed.
    week: Mutex<Option<(Instant, HistoryView)>>,
    /// Times the week was summed so far; for tests.
    weeks: AtomicUsize,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            tx: watch::Sender::new(None),
            watchers: AtomicUsize::new(0),
            running: AtomicBool::new(false),
            probe: Mutex::new(None),
            probe_now: Notify::new(),
            poll: Mutex::new(POLL),
            rounds: AtomicUsize::new(0),
            week: Mutex::new(None),
            weeks: AtomicUsize::new(0),
        }
    }
}

impl Monitor {
    /// Asks the PC and Strata again right away, e.g. after a button.
    pub fn refresh_now(&self) {
        self.probe_now.notify_one();
    }

    /// Changes how often the PC and Strata are asked; for tests.
    pub fn set_poll(&self, poll: Duration) {
        *self.poll.lock().unwrap() = poll;
    }

    pub fn rounds(&self) -> usize {
        self.rounds.load(Ordering::SeqCst)
    }

    pub fn weeks_summed(&self) -> usize {
        self.weeks.load(Ordering::SeqCst)
    }
}

/// The last 7 days of the history, summed again once `WEEK_REFRESH` passed.
async fn week(state: &Arc<AppState>) -> HistoryView {
    let monitor = &state.overview;
    if let Some((at, view)) = monitor.week.lock().unwrap().as_ref()
        && at.elapsed() < WEEK_REFRESH
    {
        return view.clone();
    }
    let view = crate::history::view(state, "7d").await;
    monitor.weeks.fetch_add(1, Ordering::SeqCst);
    *monitor.week.lock().unwrap() = Some((Instant::now(), view.clone()));
    view
}

/// Counts a watching page until dropped.
struct Watcher(Arc<AppState>);

impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.overview.watchers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The overview now and after every change; starts asking the PC and Strata
/// while anyone watches.
pub fn watch(state: &Arc<AppState>) -> BoxStream<'static, Overview> {
    let monitor = &state.overview;
    monitor.watchers.fetch_add(1, Ordering::SeqCst);
    let watcher = Watcher(Arc::clone(state));
    start(state);
    let mut rx = monitor.tx.subscribe();
    // The current overview first, if there is one; then one per change.
    rx.mark_changed();
    futures_util::stream::unfold((rx, watcher), |(mut rx, watcher)| async move {
        loop {
            rx.changed().await.ok()?;
            let current = rx.borrow_and_update().clone();
            if let Some(overview) = current {
                return Some((overview, (rx, watcher)));
            }
        }
    })
    .boxed()
}

fn start(state: &Arc<AppState>) {
    if state.overview.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        loop {
            run(&state).await;
            state.overview.running.store(false, Ordering::SeqCst);
            // A page may have come between the last check and the flag.
            if state.overview.watchers.load(Ordering::SeqCst) == 0
                || state.overview.running.swap(true, Ordering::SeqCst)
            {
                return;
            }
        }
    });
}

async fn run(state: &Arc<AppState>) {
    let monitor = &state.overview;
    let mut last_probe: Option<Instant> = None;
    // A fresh start shows nothing stale: the first overview comes after asking.
    monitor.tx.send_replace(None);
    while monitor.watchers.load(Ordering::SeqCst) > 0 {
        let job_runs = monitor
            .probe
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.job.is_some());
        let poll = *monitor.poll.lock().unwrap();
        let poll = if job_runs { poll.min(POLL_JOB) } else { poll };
        if last_probe.is_none_or(|t| t.elapsed() >= poll) {
            let probe = probe(state).await;
            *monitor.probe.lock().unwrap() = Some(probe);
            monitor.rounds.fetch_add(1, Ordering::SeqCst);
            last_probe = Some(Instant::now());
        }
        let Some(probe) = monitor.probe.lock().unwrap().clone() else {
            continue;
        };
        let week = week(state).await;
        let overview = build(state, &probe, week);
        monitor.tx.send_if_modified(|current| {
            // The clock alone is no change: the page counts down by itself.
            let unchanged = current.as_ref().is_some_and(|c| {
                c == &Overview {
                    now: c.now,
                    ..overview.clone()
                }
            });
            if unchanged {
                return false;
            }
            *current = Some(overview);
            true
        });
        let next_probe = last_probe.map_or(Duration::ZERO, |t| poll.saturating_sub(t.elapsed()));
        tokio::select! {
            _ = tokio::time::sleep(REFRESH.min(next_probe)) => {}
            _ = monitor.probe_now.notified() => last_probe = None,
        }
    }
}

async fn probe(state: &AppState) -> Probe {
    // The plug alongside, so a slow one holds up nothing else.
    let ((health, metrics, pc, job), power) = tokio::join!(probe_pc(state), power(state));
    Probe {
        health,
        metrics,
        pc,
        job,
        power,
    }
}

/// Judged from the setting at the time of asking, so a plug set up a moment
/// ago is not shown as unreachable before it was asked.
async fn power(state: &AppState) -> PowerView {
    let Some(url) = state.cfg().shelly_url.clone() else {
        return PowerView::NotConfigured;
    };
    match crate::plug::power(&state.probe_client(), &url).await {
        Some(watts) => PowerView::Watts(watts),
        None => PowerView::Unreachable,
    }
}

/// Strata's metrics, or none when it is not set up or does not answer.
async fn metrics(state: &AppState) -> Option<Metrics> {
    state.strata()?.metrics().await.ok()
}

async fn probe_pc(
    state: &AppState,
) -> (Health, Option<Metrics>, Option<PcStatus>, Option<StrataJob>) {
    let health = state.strata_health().await;
    let up = matches!(health, Health::Up { .. });
    let (metrics, pc) = tokio::join!(metrics(state), state.check_pc(up));
    let job = match &pc {
        Some(status) if status.strata_job => state
            .agent()
            .strata_job()
            .await
            .filter(|j| j.state == strata_admin_ui::api::StrataJobState::Running),
        _ => None,
    };
    (health, metrics, pc, job)
}

fn strata_view(probe: &Probe, in_flight: usize) -> StrataView {
    match (probe.metrics, probe.health) {
        (Some(m), _) if m.processing > 0 => StrataView::Busy {
            processing: m.processing,
            deferred: Some(m.deferred),
            tokens_per_sec: m.tokens_per_sec,
        },
        (None, Health::Up { .. }) if in_flight > 0 => StrataView::Busy {
            processing: in_flight as u32,
            deferred: None,
            tokens_per_sec: None,
        },
        (_, Health::Up { loaded: true }) => StrataView::Ready,
        (_, Health::Up { loaded: false }) => StrataView::Unloaded,
        // The process runs but does not answer yet: starting, or restarted by
        // the watchdog.
        _ if probe
            .pc
            .as_ref()
            .is_some_and(|s| s.strata_running && !s.gaming_mode) =>
        {
            StrataView::Loading
        }
        _ => StrataView::Offline,
    }
}

/// How far the proxy is set up, from the configuration alone.
pub fn setup(state: &AppState) -> Setup {
    let cfg = state.cfg();
    Setup {
        configured: cfg.strata_url.is_some(),
        mac_known: cfg.pc_mac.is_some(),
        agent_configured: cfg.pc_agent_url.is_some(),
        api_key_exists: !state.api_keys.is_empty(),
        persistent: state.settings.persistent(),
    }
}

/// Values only while the PC counts as awake: none from before the sleep request,
/// also not during settling, when the PC may still answer.
fn hardware_view(pc: PcView, status: Option<&PcStatus>) -> HardwareView {
    match (pc, status) {
        (PcView::Awake | PcView::Gaming, Some(status)) => match &status.hardware {
            Some(hardware) => HardwareView::Values(hardware.clone()),
            None => HardwareView::Unsupported,
        },
        _ => HardwareView::Unavailable,
    }
}

fn build(state: &AppState, probe: &Probe, week: HistoryView) -> Overview {
    let cfg = state.cfg();
    let snap = state.snapshot();
    let now = SystemTime::now();
    let strata = strata_view(probe, snap.in_flight);
    let pc = PcView::from(snap.pc);
    let keep_awake_until = snap
        .keep_awake_until
        .map(|t| clock::local_text(clock::unix(t)));
    let sleep = if cfg.pc_agent_url.is_none() {
        SleepView::NoAgent
    } else {
        match pc {
            PcView::Asleep => SleepView::Asleep,
            PcView::Waking => SleepView::Waking,
            PcView::Gaming => SleepView::Gaming,
            PcView::Unknown => SleepView::Unknown,
            PcView::Awake => match (&keep_awake_until, &snap.refused) {
                (Some(until), _) => SleepView::KeptAwake {
                    until: until.clone(),
                },
                // Only requests through the proxy hold off the sleep request;
                // others make the PC service refuse it, which shows up below.
                _ if snap.in_flight > 0 => SleepView::Busy,
                (None, Some(r)) if r.retry_at > now => SleepView::Refused {
                    reason: r.reason.clone(),
                    retry_at: clock::unix(r.retry_at),
                },
                _ => SleepView::At(clock::unix(snap.last_activity_at + cfg.idle_timeout)),
            },
        }
    };
    let (mut pc_service, timed_out) = state.pc_setup.view();
    if let Some(service) = &mut pc_service {
        service.reported_mac = state.settings.reported_mac().map(crate::config::format_mac);
    }
    if let Some(reason) = timed_out {
        state.events.record(
            crate::events::EventKind::PcServiceUpdateFailed,
            Some(reason),
        );
    }
    let events = state
        .events
        .latest_pc(EVENTS_SHOWN)
        .into_iter()
        .map(|e| EventView {
            time: clock::local_text(e.ts),
            kind: e.kind,
            detail: e.detail,
        })
        .collect();
    Overview {
        now: clock::unix(now),
        setup: setup(state),
        idle_timeout_secs: cfg.idle_timeout.as_secs(),
        strata_job: probe.job.clone(),
        pc,
        strata,
        last_request: snap.last_request.map(|t| clock::local_text(clock::unix(t))),
        sleep,
        keep_awake_until,
        events,
        pc_service,
        hardware: hardware_view(pc, probe.pc.as_ref()),
        power: probe.power.clone(),
        week,
    }
}
