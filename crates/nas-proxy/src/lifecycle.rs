//! The PC lifecycle: every decision about the PC's Zustand, the Leerlaufzeit,
//! the Nachlauf after an accepted Schlafen-Anfrage, refusals and Wach halten.
//!
//! It does no I/O and reads no clock. The shell in `state` asks the PC-Dienst
//! and Strata, sends magic packets and tells this module what it saw, with
//! the time as a [`Now`] and the durations of the current Config as
//! [`Timings`]. Each call hands back the Ereignis to record, if any; the shell
//! records it while it still holds the lock, so the Verlauf keeps the order
//! of changes. [`state_after`] reads those Ereignisse back for the Verlauf.
//!
//! Ereignisse that change no Zustand (WakeSent, WakeRequested, MacLearned,
//! PcServiceUpdated) stay with the I/O that causes them.

use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use strata_admin_ui::api::PcView;
use strata_pc_protocol::PcStatus;

use crate::{agent::SleepAnswer, config::Config, events::EventKind};

/// Healthy answers younger than this skip the `/health` check.
const HEALTH_CACHE: Duration = Duration::from_secs(5);

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

/// The PC service refused to sleep.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub reason: String,
    /// When it is asked again.
    pub retry_at: SystemTime,
}

/// The time of a call, read once by the shell.
#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub at: Instant,
    /// The same moment as wall-clock time, for what is shown or saved.
    pub wall: SystemTime,
}

impl Now {
    pub fn read() -> Self {
        Self {
            at: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// The durations of the current Config; settings can change while the proxy
/// runs, so each call gets them anew.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    /// The Leerlaufzeit.
    pub idle_timeout: Duration,
    /// The Nachlauf.
    pub settle: Duration,
    /// How long a refused or failed Schlafen-Anfrage waits.
    pub retry: Duration,
}

impl From<&Config> for Timings {
    fn from(cfg: &Config) -> Self {
        Self {
            idle_timeout: cfg.idle_timeout,
            settle: cfg.sleep_settle,
            retry: cfg.sleep_retry,
        }
    }
}

/// An Ereignis to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: EventKind,
    pub detail: Option<String>,
}

impl Record {
    fn new(kind: EventKind, detail: Option<String>) -> Self {
        Self { kind, detail }
    }
}

/// What an answer of the PC service or of Strata meant.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub struct Observed {
    /// The PC service: the PC is up and not in gaming mode. Strata: its
    /// answer counts (none during the Nachlauf).
    pub up: bool,
    pub record: Option<Record>,
}

/// What the overview needs from the lifecycle.
pub struct Snapshot {
    pub pc: PcState,
    pub in_flight: usize,
    pub last_activity_at: SystemTime,
    pub last_request: Option<SystemTime>,
    pub refused: Option<Refusal>,
    pub keep_awake_until: Option<SystemTime>,
}

pub struct Lifecycle {
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
    /// Unknown is the Zustand since the start, not that of a wake-up that
    /// failed.
    unknown_since_start: bool,
}

impl Lifecycle {
    /// Just started: nothing seen yet, the Leerlaufzeit runs from now.
    pub fn new(now: Now, keep_awake_until: Option<SystemTime>) -> Self {
        Self {
            pc: PcState::Unknown,
            last_activity: now.at,
            last_activity_at: now.wall,
            last_request: None,
            last_healthy: None,
            sleep_accepted_at: None,
            next_sleep_attempt: None,
            refused: None,
            keep_awake_until,
            unknown_since_start: true,
        }
    }

    pub fn pc(&self) -> PcState {
        self.pc
    }

    pub fn snapshot(&self, in_flight: usize, now: Now) -> Snapshot {
        Snapshot {
            pc: self.pc,
            in_flight,
            last_activity_at: self.last_activity_at,
            last_request: self.last_request,
            refused: self.refused.clone(),
            keep_awake_until: self.keep_awake_until.filter(|t| *t > now.wall),
        }
    }

    /// True while a PC that just accepted a sleep request may still answer.
    pub fn settling(&self, now: Now, t: Timings) -> bool {
        self.sleep_accepted_at
            .is_some_and(|at| now.at.saturating_duration_since(at) < t.settle)
    }

    /// Notes the PC service's answer: gaming mode, a sleeping PC (no answer,
    /// unless Strata answers or a wake-up runs) or a PC that is up.
    pub fn pc_answered(
        &mut self,
        status: Option<&PcStatus>,
        strata_answers: bool,
        now: Now,
        t: Timings,
    ) -> Observed {
        let from = self.pc;
        let up = match status {
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
            Some(_) if self.settling(now, t) => false,
            // The wake-up ends once Strata answers.
            Some(_) if self.pc == PcState::Waking => true,
            Some(_) => {
                self.mark_awake(now);
                true
            }
        };
        Observed {
            up,
            record: self.seen(from),
        }
    }

    /// Notes whether Strata answered `/health`. A ready answer marks the PC
    /// awake, except while a PC that accepted sleep may still answer.
    pub fn strata_answered(&mut self, healthy: bool, now: Now, t: Timings) -> Observed {
        if self.settling(now, t) {
            return Observed {
                up: false,
                record: None,
            };
        }
        let from = self.pc;
        if healthy {
            self.last_healthy = Some(now.at);
            self.mark_awake(now);
        }
        Observed {
            up: healthy,
            record: self.seen(from),
        }
    }

    /// A healthy answer is fresh enough to skip asking Strata again.
    pub fn healthy_lately(&self, now: Now, t: Timings) -> bool {
        !self.settling(now, t)
            && self
                .last_healthy
                .is_some_and(|at| now.at.saturating_duration_since(at) < HEALTH_CACHE)
    }

    /// The PC service reported gaming mode outside the regular checks.
    pub fn seen_gaming(&mut self) -> Option<Record> {
        let from = self.pc;
        self.pc = PcState::Gaming;
        self.seen(from)
    }

    /// Whether to ask the PC to sleep now: the PC is up, nothing ran for the
    /// Leerlaufzeit, no Wach halten, and no refusal or failure waits.
    pub fn wants_sleep(&self, in_flight: usize, now: Now, t: Timings) -> bool {
        in_flight == 0
            && now.at.saturating_duration_since(self.last_activity) >= t.idle_timeout
            && !self.kept_awake(now)
            && self.next_sleep_attempt.is_none_or(|at| at <= now.at)
    }

    /// Notes the answer to a Schlafen-Anfrage; `None` when none came. A
    /// refusal or a missing answer postpones the next attempt by the retry
    /// wait.
    pub fn sleep_answered(
        &mut self,
        answer: Option<&SleepAnswer>,
        now: Now,
        t: Timings,
    ) -> Option<Record> {
        match answer {
            Some(SleepAnswer::Accepted) => {
                self.pc = PcState::Asleep;
                self.sleep_accepted_at = Some(now.at);
                self.last_healthy = None;
                self.next_sleep_attempt = None;
                self.refused = None;
                Some(Record::new(EventKind::SleepAccepted, None))
            }
            Some(SleepAnswer::Refused(reason)) => {
                self.next_sleep_attempt = Some(now.at + t.retry);
                self.refused = Some(Refusal {
                    reason: reason.clone(),
                    retry_at: now.wall + t.retry,
                });
                Some(Record::new(EventKind::SleepRefused, Some(reason.clone())))
            }
            None => {
                self.next_sleep_attempt = Some(now.at + t.retry);
                None
            }
        }
    }

    /// The proxy starts sending magic packets.
    pub fn wake_started(&mut self) {
        self.pc = PcState::Waking;
        self.unknown_since_start = false;
    }

    /// The wake-up the proxy started reached the PC after `waited`; the
    /// Leerlaufzeit starts now. A Zustand noticed meanwhile, e.g. gaming
    /// mode, stays.
    pub fn woke(&mut self, waited: Duration, now: Now) -> Record {
        if self.pc == PcState::Waking {
            self.pc = PcState::Awake;
        }
        self.touch(now);
        Record::new(
            EventKind::Awake,
            Some(format!("after {:.1} s", waited.as_secs_f64())),
        )
    }

    /// The PC was up before any magic packet, e.g. woken at the desk.
    pub fn woken_elsewhere(&mut self) -> Option<Record> {
        let from = self.pc;
        self.pc = PcState::Awake;
        self.seen(from)
    }

    /// The wake-up ended without the PC.
    pub fn wake_timed_out(&mut self) -> Record {
        self.pc = PcState::Unknown;
        Record::new(EventKind::WakeTimeout, None)
    }

    /// No magic packet could be sent.
    pub fn wake_failed(&mut self) {
        self.pc = PcState::Unknown;
    }

    /// Gaming mode was switched by `by`; recorded only if that changed the
    /// Zustand. Off starts the Leerlaufzeit again, so the PC sleeps after the
    /// session.
    pub fn gaming_switched(&mut self, on: bool, by: &str, now: Now) -> Option<Record> {
        let from = self.pc;
        let (to, kind) = if on {
            (PcState::Gaming, EventKind::GamingOn)
        } else {
            self.touch(now);
            (PcState::Awake, EventKind::GamingOff)
        };
        self.pc = to;
        (from != to).then(|| Record::new(kind, Some(by.into())))
    }

    /// Starts the Leerlaufzeit again from now, e.g. after waking by hand.
    pub fn touch(&mut self, now: Now) {
        self.last_activity = now.at;
        self.last_activity_at = now.wall;
        self.refused = None;
    }

    /// A forwarded request started or ended.
    pub fn request_seen(&mut self, now: Now) {
        self.touch(now);
        self.last_request = Some(now.wall);
    }

    /// Keeps the PC awake until `until`, or ends that with `None`.
    pub fn set_keep_awake(&mut self, until: Option<SystemTime>) {
        self.keep_awake_until = until;
    }

    fn kept_awake(&self, now: Now) -> bool {
        self.keep_awake_until.is_some_and(|t| t > now.wall)
    }

    /// The PC is up. Woken, gaming just ended or up after a failed wake-up:
    /// the Leerlaufzeit starts now. Unknown since the start: it keeps
    /// running from the start.
    fn mark_awake(&mut self, now: Now) {
        match self.pc {
            PcState::Awake => {}
            PcState::Unknown if self.unknown_since_start => self.pc = PcState::Awake,
            _ => {
                self.pc = PcState::Awake;
                self.touch(now);
            }
        }
    }

    /// The Ereignis of a Zustand the proxy noticed on its own, if it changed
    /// since `from`.
    fn seen(&self, from: PcState) -> Option<Record> {
        if self.pc == from {
            return None;
        }
        let kind = match self.pc {
            // The wake-up records Awake with its time itself.
            PcState::Awake if from == PcState::Waking => return None,
            PcState::Awake => EventKind::PcSeenAwake,
            PcState::Asleep => EventKind::PcSeenAsleep,
            PcState::Gaming => EventKind::PcSeenGaming,
            // Steps on the way; the Verlauf reads them from the Ereignisse
            // around them.
            PcState::Waking | PcState::Unknown => return None,
        };
        Some(Record::new(kind, None))
    }
}

/// What an Ereignis says about the Zustand from then on, for the Verlauf;
/// `None` for one that says nothing about it. The counterpart of the
/// Ereignisse this module hands out.
pub fn state_after(kind: EventKind) -> Option<PcState> {
    match kind {
        EventKind::Awake | EventKind::PcSeenAwake | EventKind::GamingOff => Some(PcState::Awake),
        EventKind::SleepAccepted | EventKind::PcSeenAsleep => Some(PcState::Asleep),
        EventKind::PcSeenGaming | EventKind::GamingOn => Some(PcState::Gaming),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Timings = Timings {
        idle_timeout: Duration::from_secs(900),
        settle: Duration::from_secs(30),
        retry: Duration::from_secs(60),
    };

    /// A fixed start and moments after it.
    struct Clock(Now);

    impl Clock {
        fn new() -> Self {
            Self(Now::read())
        }

        fn at(&self, secs: u64) -> Now {
            let d = Duration::from_secs(secs);
            Now {
                at: self.0.at + d,
                wall: self.0.wall + d,
            }
        }
    }

    fn up() -> PcStatus {
        PcStatus::default()
    }

    fn gaming() -> PcStatus {
        PcStatus {
            gaming_mode: true,
            ..PcStatus::default()
        }
    }

    fn seen(kind: EventKind) -> Option<Record> {
        Some(Record::new(kind, None))
    }

    /// Awake, last activity at second 0.
    fn awake(c: &Clock) -> Lifecycle {
        let mut l = Lifecycle::new(c.at(0), None);
        let _ = l.pc_answered(Some(&up()), false, c.at(0), T);
        assert_eq!(l.pc(), PcState::Awake);
        l
    }

    /// Accepted sleep at second 1000.
    fn asleep(c: &Clock) -> Lifecycle {
        let mut l = awake(c);
        let _ = l.sleep_answered(Some(&SleepAnswer::Accepted), c.at(1000), T);
        l
    }

    #[test]
    fn pc_seen_up_after_start_counts_idle_time_from_the_start() {
        let c = Clock::new();
        let mut l = Lifecycle::new(c.at(0), None);
        let seen_up = l.pc_answered(Some(&up()), false, c.at(600), T);
        assert_eq!(
            seen_up,
            Observed {
                up: true,
                record: seen(EventKind::PcSeenAwake)
            }
        );
        assert!(!l.wants_sleep(0, c.at(899), T));
        assert!(l.wants_sleep(0, c.at(900), T));
    }

    #[test]
    fn pc_seen_up_after_sleep_starts_idle_time_again() {
        let c = Clock::new();
        let mut l = asleep(&c);
        let seen_up = l.pc_answered(Some(&up()), false, c.at(2000), T);
        assert_eq!(seen_up.record, seen(EventKind::PcSeenAwake));
        assert!(!l.wants_sleep(0, c.at(2899), T));
        assert!(l.wants_sleep(0, c.at(2900), T));
    }

    #[test]
    fn an_unchanged_state_records_nothing() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert_eq!(l.pc_answered(Some(&up()), false, c.at(10), T).record, None);
        assert_eq!(
            l.pc_answered(Some(&gaming()), false, c.at(20), T).record,
            seen(EventKind::PcSeenGaming)
        );
        assert_eq!(
            l.pc_answered(Some(&gaming()), false, c.at(30), T).record,
            None
        );
        assert_eq!(l.seen_gaming(), None);
    }

    #[test]
    fn gaming_mode_is_not_up() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert!(!l.pc_answered(Some(&gaming()), false, c.at(10), T).up);
        assert_eq!(l.pc(), PcState::Gaming);
    }

    #[test]
    fn no_answer_is_asleep_unless_strata_answers_or_a_wake_up_runs() {
        let c = Clock::new();
        let mut l = awake(&c);
        let missed = l.pc_answered(None, true, c.at(10), T);
        assert_eq!((missed.up, missed.record), (false, None));
        assert_eq!(l.pc(), PcState::Awake);

        l.wake_started();
        assert_eq!(l.pc_answered(None, false, c.at(20), T).record, None);
        assert_eq!(l.pc(), PcState::Waking);

        let mut l = awake(&c);
        assert_eq!(
            l.pc_answered(None, false, c.at(30), T).record,
            seen(EventKind::PcSeenAsleep)
        );
        assert_eq!(l.pc(), PcState::Asleep);
    }

    #[test]
    fn an_answer_during_a_wake_up_is_up_but_leaves_it_to_strata() {
        let c = Clock::new();
        let mut l = awake(&c);
        l.wake_started();
        let answer = l.pc_answered(Some(&up()), false, c.at(10), T);
        assert_eq!((answer.up, answer.record), (true, None));
        assert_eq!(l.pc(), PcState::Waking);
    }

    #[test]
    fn idle_time_in_flight_and_keep_awake_hold_sleep_off() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert!(!l.wants_sleep(0, c.at(899), T), "Leerlaufzeit not over");
        assert!(l.wants_sleep(0, c.at(900), T));
        assert!(!l.wants_sleep(1, c.at(900), T), "a request runs");

        l.set_keep_awake(Some(c.at(1000).wall));
        assert!(!l.wants_sleep(0, c.at(999), T), "Wach halten");
        assert!(l.wants_sleep(0, c.at(1000), T), "Wach halten ended");
    }

    #[test]
    fn requests_and_touch_start_idle_time_again() {
        let c = Clock::new();
        let mut l = awake(&c);
        l.request_seen(c.at(500));
        assert!(!l.wants_sleep(0, c.at(1399), T));
        assert!(l.wants_sleep(0, c.at(1400), T));
        assert_eq!(l.snapshot(0, c.at(500)).last_request, Some(c.at(500).wall));

        l.touch(c.at(2000));
        assert!(!l.wants_sleep(0, c.at(2899), T));
        assert_eq!(l.snapshot(0, c.at(2000)).last_activity_at, c.at(2000).wall);
    }

    #[test]
    fn an_accepted_sleep_request_is_asleep_and_settles() {
        let c = Clock::new();
        let mut l = awake(&c);
        let _ = l.strata_answered(true, c.at(999), T);
        assert_eq!(
            l.sleep_answered(Some(&SleepAnswer::Accepted), c.at(1000), T),
            seen(EventKind::SleepAccepted)
        );
        assert_eq!(l.pc(), PcState::Asleep);
        assert!(!l.healthy_lately(c.at(1001), T), "no cached health");
        assert!(l.settling(c.at(1029), T));
        assert!(!l.settling(c.at(1030), T));
    }

    #[test]
    fn during_the_nachlauf_answers_do_not_count() {
        let c = Clock::new();
        let mut l = asleep(&c);
        let pc = l.pc_answered(Some(&up()), false, c.at(1029), T);
        assert_eq!((pc.up, pc.record), (false, None));
        let strata = l.strata_answered(true, c.at(1029), T);
        assert_eq!((strata.up, strata.record), (false, None));
        assert_eq!(l.pc(), PcState::Asleep);

        assert_eq!(
            l.strata_answered(true, c.at(1030), T),
            Observed {
                up: true,
                record: seen(EventKind::PcSeenAwake)
            }
        );
    }

    #[test]
    fn a_refusal_waits_and_is_shown_until_something_happens() {
        let c = Clock::new();
        let mut l = awake(&c);
        let refused = SleepAnswer::Refused("Spiel läuft".into());
        assert_eq!(
            l.sleep_answered(Some(&refused), c.at(900), T),
            Some(Record::new(
                EventKind::SleepRefused,
                Some("Spiel läuft".into())
            ))
        );
        assert_eq!(l.pc(), PcState::Awake);
        let shown = l.snapshot(0, c.at(900)).refused.unwrap();
        assert_eq!(shown.reason, "Spiel läuft");
        assert_eq!(shown.retry_at, c.at(960).wall);
        assert!(!l.wants_sleep(0, c.at(959), T));
        assert!(l.wants_sleep(0, c.at(960), T));

        l.request_seen(c.at(970));
        assert!(l.snapshot(0, c.at(970)).refused.is_none());
    }

    #[test]
    fn a_missing_answer_waits_without_a_refusal() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert_eq!(l.sleep_answered(None, c.at(900), T), None);
        assert!(l.snapshot(0, c.at(900)).refused.is_none());
        assert!(!l.wants_sleep(0, c.at(959), T));
        assert!(l.wants_sleep(0, c.at(960), T));
    }

    #[test]
    fn healthy_strata_is_cached_for_five_seconds() {
        let c = Clock::new();
        let mut l = Lifecycle::new(c.at(0), None);
        assert!(!l.healthy_lately(c.at(0), T));
        assert_eq!(
            l.strata_answered(true, c.at(10), T).record,
            seen(EventKind::PcSeenAwake)
        );
        assert!(l.healthy_lately(c.at(14), T));
        assert!(!l.healthy_lately(c.at(15), T));
        let down = l.strata_answered(false, c.at(20), T);
        assert_eq!((down.up, down.record), (false, None));
        assert_eq!(l.pc(), PcState::Awake);
    }

    #[test]
    fn wake_ups_end_awake_or_unknown() {
        let c = Clock::new();
        let mut l = asleep(&c);
        l.wake_started();
        assert_eq!(l.pc(), PcState::Waking);
        assert_eq!(
            l.woke(Duration::from_millis(4200), c.at(2000)),
            Record::new(EventKind::Awake, Some("after 4.2 s".into()))
        );
        assert_eq!(l.pc(), PcState::Awake);

        l.wake_started();
        assert_eq!(
            l.wake_timed_out(),
            Record::new(EventKind::WakeTimeout, None)
        );
        assert_eq!(l.pc(), PcState::Unknown);

        l.wake_started();
        l.wake_failed();
        assert_eq!(l.pc(), PcState::Unknown);

        let mut l = asleep(&c);
        assert_eq!(l.woken_elsewhere(), seen(EventKind::PcSeenAwake));
        assert_eq!(l.woken_elsewhere(), None);
    }

    #[test]
    fn a_wake_up_keeps_gaming_mode_seen_meanwhile() {
        let c = Clock::new();
        let mut l = asleep(&c);
        l.wake_started();
        let _ = l.seen_gaming();
        let _ = l.woke(Duration::from_secs(5), c.at(2000));
        assert_eq!(l.pc(), PcState::Gaming);
    }

    #[test]
    fn gaming_off_starts_idle_time_again() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert_eq!(
            l.gaming_switched(true, "from the web UI", c.at(100)),
            Some(Record::new(
                EventKind::GamingOn,
                Some("from the web UI".into())
            ))
        );
        assert_eq!(l.pc(), PcState::Gaming);
        assert_eq!(
            l.gaming_switched(false, "from the web UI", c.at(5000)),
            Some(Record::new(
                EventKind::GamingOff,
                Some("from the web UI".into())
            ))
        );
        assert_eq!(l.pc(), PcState::Awake);
        assert!(!l.wants_sleep(0, c.at(5899), T));
        assert!(l.wants_sleep(0, c.at(5900), T));
    }

    #[test]
    fn gaming_switched_to_what_it_was_records_nothing() {
        let c = Clock::new();
        let mut l = awake(&c);
        assert_eq!(l.gaming_switched(false, "x", c.at(100)), None);
        assert!(l.gaming_switched(true, "x", c.at(200)).is_some());
        assert_eq!(l.gaming_switched(true, "x", c.at(300)), None);
        assert_eq!(l.pc(), PcState::Gaming);
    }

    #[test]
    fn a_wake_up_records_awake_once() {
        let c = Clock::new();
        let mut l = asleep(&c);
        l.wake_started();
        // The wake-up's own check of Strata.
        let strata = l.strata_answered(true, c.at(2000), T);
        assert_eq!((strata.up, strata.record), (true, None));
        assert_eq!(l.pc(), PcState::Awake);
        assert_eq!(
            l.woke(Duration::from_secs(5), c.at(2000)).kind,
            EventKind::Awake
        );
    }

    #[test]
    fn a_wake_up_starts_idle_time_again_also_into_gaming_mode() {
        let c = Clock::new();
        let mut l = asleep(&c);
        l.wake_started();
        let _ = l.woke(Duration::from_secs(5), c.at(2000));
        assert!(!l.wants_sleep(0, c.at(2899), T));
        assert!(l.wants_sleep(0, c.at(2900), T));

        let mut l = asleep(&c);
        l.wake_started();
        let _ = l.seen_gaming();
        let _ = l.woke(Duration::from_secs(5), c.at(2000));
        assert_eq!(l.pc(), PcState::Gaming);
        assert_eq!(l.snapshot(0, c.at(2000)).last_activity_at, c.at(2000).wall);
    }

    #[test]
    fn a_pc_up_after_a_failed_wake_up_starts_idle_time_again() {
        let c = Clock::new();
        let mut l = asleep(&c);
        l.wake_started();
        let _ = l.wake_timed_out();
        let seen_up = l.pc_answered(Some(&up()), false, c.at(2000), T);
        assert_eq!(seen_up.record, seen(EventKind::PcSeenAwake));
        assert!(!l.wants_sleep(0, c.at(2899), T));
        assert!(l.wants_sleep(0, c.at(2900), T));

        let mut l = asleep(&c);
        l.wake_started();
        l.wake_failed();
        let _ = l.strata_answered(true, c.at(2000), T);
        assert!(!l.wants_sleep(0, c.at(2899), T));
    }

    #[test]
    fn the_snapshot_shows_keep_awake_only_while_it_lasts() {
        let c = Clock::new();
        let l = Lifecycle::new(c.at(0), Some(c.at(100).wall));
        assert_eq!(
            l.snapshot(2, c.at(99)).keep_awake_until,
            Some(c.at(100).wall)
        );
        assert_eq!(l.snapshot(2, c.at(99)).in_flight, 2);
        assert_eq!(l.snapshot(2, c.at(100)).keep_awake_until, None);
    }

    type Step<'a> = Box<dyn Fn(&mut Lifecycle) -> Option<Record> + 'a>;

    /// Every Ereignis the lifecycle hands out tells the Verlauf the Zustand it
    /// left behind. One that tells nothing keeps the Zustand, or leaves a step
    /// on the way that the Verlauf reads from the Ereignisse around it.
    #[test]
    fn the_verlauf_reads_back_what_the_lifecycle_records() {
        let c = Clock::new();
        let mut l = Lifecycle::new(c.at(0), None);
        let mut kinds = Vec::new();
        let steps: Vec<Step> = vec![
            Box::new(|l| l.pc_answered(Some(&up()), false, c.at(1), T).record),
            Box::new(|l| l.pc_answered(Some(&gaming()), false, c.at(2), T).record),
            Box::new(|l| l.pc_answered(None, false, c.at(3), T).record),
            Box::new(|l| l.strata_answered(true, c.at(4), T).record),
            Box::new(|l| l.seen_gaming()),
            Box::new(|l| l.gaming_switched(false, "x", c.at(5))),
            Box::new(|l| l.gaming_switched(true, "x", c.at(6))),
            Box::new(|l| l.woken_elsewhere()),
            Box::new(|l| l.sleep_answered(Some(&SleepAnswer::Refused("x".into())), c.at(7), T)),
            Box::new(|l| l.sleep_answered(Some(&SleepAnswer::Accepted), c.at(8), T)),
            Box::new(|l| {
                l.wake_started();
                Some(l.woke(Duration::from_secs(9), c.at(9)))
            }),
            Box::new(|l| Some(l.wake_timed_out())),
        ];
        for step in &steps {
            let before = l.pc();
            let record = step(&mut l).expect("each step records");
            match state_after(record.kind) {
                Some(state) => assert_eq!(state, l.pc(), "{record:?}"),
                None => assert!(
                    l.pc() == before || matches!(l.pc(), PcState::Unknown | PcState::Waking),
                    "{record:?} tells nothing, but left {:?}",
                    l.pc()
                ),
            }
            kinds.push(record.kind);
        }
        kinds.sort_by_key(|k| format!("{k:?}"));
        kinds.dedup();
        assert_eq!(kinds.len(), 9, "every Ereignis the lifecycle can hand out");
    }
}
