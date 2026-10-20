//! The history: how long the PC was in which state on each day, week or month
//! of a range. The calendar arithmetic is pure - events, range and time zone
//! in, hours per bar out - so its boundaries can be tested without a running
//! proxy. The days follow the NAS's time zone, as `clock::zone` names it.

use std::sync::Arc;

use jiff::{
    Timestamp, ToSpan,
    civil::{Date, Time},
    tz::TimeZone,
};
use strata_admin_ui::api::{
    Energy, EnergySource, EventKind, HistoryBucket, HistoryDay, HistoryTotals, HistoryView,
    PowerSettings,
};

use crate::{
    clock,
    events::{Event, HourSamples},
    lifecycle::{self, PcState},
    state::AppState,
};

/// Up to this many days the history draws one bar per day.
const DAY_BARS_UP_TO: usize = 62;
/// Up to this many days one per week, beyond that one per month.
const WEEK_BARS_UP_TO: usize = 366;

/// The earliest day a picked range shows.
const EARLIEST: Date = jiff::civil::date(2000, 1, 1);

/// The events that say something about the PC's state.
const STATE_KINDS: &[EventKind] = &[
    EventKind::Awake,
    EventKind::PcSeenAwake,
    EventKind::GamingOff,
    EventKind::SleepAccepted,
    EventKind::PcSeenAsleep,
    EventKind::PcSeenGaming,
    EventKind::GamingOn,
    EventKind::ProxyStarted,
    EventKind::ProxyStopped,
    EventKind::WakeTimeout,
];

/// The state of the PC, as the events describe it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Awake,
    Gaming,
    Asleep,
    Unknown,
}

/// A span of time in which the PC was in one state.
struct Period {
    from: i64,
    to: i64,
    state: State,
}

/// What an event says about the state from then on; `None` for one that
/// says nothing about it. The mapping sits next to the transitions that
/// write the events.
fn state_after(kind: EventKind) -> Option<State> {
    lifecycle::state_after(kind).map(|pc| match pc {
        PcState::Awake => State::Awake,
        PcState::Gaming => State::Gaming,
        PcState::Asleep => State::Asleep,
        PcState::Waking | PcState::Unknown => State::Unknown,
    })
}

/// The day this time belongs to in `tz`.
fn day_of(ts: i64, tz: &TimeZone) -> Option<Date> {
    Timestamp::from_second(ts)
        .ok()
        .map(|t| t.to_zoned(tz.clone()).date())
}

/// The start of this day in `tz`; a day without a midnight has none.
fn day_start(day: Date, tz: &TimeZone) -> Option<i64> {
    day.to_datetime(Time::constant(0, 0, 0, 0))
        .to_zoned(tz.clone())
        .ok()
        .map(|z| z.timestamp().as_second())
}

/// The calendar days the range `from..to` touches. Each begins at its own
/// midnight, so a day with a clock change has 23 or 25 hours; a day that only
/// begins at `to` is not one of them.
fn days(from: i64, to: i64, tz: &TimeZone) -> Vec<Date> {
    let (Some(first), Some(last)) = (day_of(from, tz), day_of(to, tz)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut day = first;
    while day <= last {
        if day_start(day, tz).is_some_and(|start| start < to) {
            out.push(day);
        }
        let Some(next) = day.tomorrow().ok() else {
            break;
        };
        day = next;
    }
    out
}

/// The spans of each state between `from` and `to`. The events before the
/// range count too: the PC's state when it begins is in the last event
/// before it.
fn periods(events: &[Event], from: i64, to: i64) -> Vec<Period> {
    let mut order: Vec<&Event> = events.iter().collect();
    order.sort_by_key(|event| event.ts);
    let mut out: Vec<Period> = Vec::new();
    // At the start of the range the proxy has seen nothing.
    let mut state = State::Unknown;
    let mut began = from;
    for event in order {
        let ts = i64::try_from(event.ts).unwrap_or(i64::MAX);
        if ts > to {
            break;
        }
        match state_after(event.kind) {
            Some(next) => {
                add(&mut out, state, began, ts, from, to);
                state = next;
                began = ts;
            }
            // No state reaches across a start of the proxy. Without a
            // `ProxyStopped` before it (a run without a beat) it is not
            // known when that run ended: what held then ends at the very
            // event that set it.
            None if event.kind == EventKind::ProxyStarted => state = State::Unknown,
            // Nothing is observed until the next known state: after a
            // waking without an answer, and from the last beat of a stopped
            // proxy on.
            None if matches!(event.kind, EventKind::WakeTimeout | EventKind::ProxyStopped) => {
                add(&mut out, state, began, ts, from, to);
                state = State::Unknown;
                began = ts;
            }
            _ => {}
        }
    }
    add(&mut out, state, began, to, from, to);
    out
}

/// Adds this span, cut to the range; one that lies entirely outside it is
/// left out.
fn add(out: &mut Vec<Period>, state: State, from: i64, to: i64, range_from: i64, range_to: i64) {
    let (from, to) = (from.max(range_from), to.min(range_to));
    if to > from {
        out.push(Period { from, to, state });
    }
}

/// The bar size for a range over this many days.
pub fn bucket_for(days: usize) -> HistoryBucket {
    if days <= DAY_BARS_UP_TO {
        HistoryBucket::Day
    } else if days <= WEEK_BARS_UP_TO {
        HistoryBucket::Week
    } else {
        HistoryBucket::Month
    }
}

/// The first day of the bar `day` belongs to; weeks begin on Monday.
fn bucket_start(day: Date, bucket: HistoryBucket) -> Date {
    match bucket {
        HistoryBucket::Day => day,
        HistoryBucket::Week => day
            .checked_sub(i64::from(day.weekday().to_monday_zero_offset()).days())
            .unwrap_or(day),
        HistoryBucket::Month => day.first_of_month(),
    }
}

/// The first day of the next bar.
fn bucket_end(start: Date, bucket: HistoryBucket) -> Option<Date> {
    match bucket {
        HistoryBucket::Day => start.tomorrow().ok(),
        HistoryBucket::Week => start.checked_add(7.days()).ok(),
        HistoryBucket::Month => start.first_of_month().checked_add(1.month()).ok(),
    }
}

fn bucket_label(day: Date, bucket: HistoryBucket) -> String {
    let format = match bucket {
        HistoryBucket::Day => "%a %d %b",
        HistoryBucket::Week => "%d %b",
        HistoryBucket::Month => "%b %Y",
    };
    day.strftime(format).to_string()
}

/// Hours in each state for every day from `from` to `to`, in the time zone
/// `tz`, without readings or settings.
pub fn day_hours(events: &[Event], from: i64, to: i64, tz: &TimeZone) -> Vec<HistoryDay> {
    let power = PowerSettings::default();
    bucket_hours(events, &[], 60, &power, from, to, tz, HistoryBucket::Day).0
}

/// Where the energy of a period came from.
#[derive(Clone, Copy, Debug, Default)]
struct Sources {
    measured: bool,
    estimated: bool,
}

impl Sources {
    fn add(&mut self, other: Sources) {
        self.measured |= other.measured;
        self.estimated |= other.estimated;
    }

    /// A period without any energy, e.g. only unknown time, counts as
    /// estimated.
    fn source(self) -> EnergySource {
        match (self.measured, self.estimated) {
            (true, true) => EnergySource::Mixed,
            (true, false) => EnergySource::Measured,
            _ => EnergySource::Estimated,
        }
    }
}

/// The energy from `start` to `end` in kWh, hour by hour: an hour whose
/// readings cover at least half of it (one per `step` seconds) counts with
/// its mean power, any other by the energy estimate from its states. Unknown
/// time without readings counts nothing. `hourly` is sorted by hour.
fn consumption(
    spans: &[Period],
    hourly: &[HourSamples],
    step: i64,
    power: &PowerSettings,
    start: i64,
    end: i64,
) -> (f64, Sources) {
    let mut kwh = 0.0;
    let mut sources = Sources::default();
    let mut hour = start - start.rem_euclid(3600);
    while hour < end {
        let (from, to) = (hour.max(start), (hour + 3600).min(end));
        let found = hourly
            .binary_search_by_key(&hour, |h| h.hour as i64)
            .ok()
            .map(|i| hourly[i])
            .filter(|h| h.slots as i64 * step.max(1) * 2 >= 3600);
        if let Some(measured) = found {
            kwh += measured.mean_watts * (to - from) as f64 / 3600.0 / 1000.0;
            sources.measured = true;
        } else {
            for span in spans {
                let seconds = (span.to.min(to) - span.from.max(from)).max(0) as f64;
                let watts = match span.state {
                    State::Awake | State::Gaming => power.awake_watts,
                    State::Asleep => power.asleep_watts,
                    State::Unknown => continue,
                };
                if seconds > 0.0 {
                    kwh += watts * seconds / 3600.0 / 1000.0;
                    sources.estimated = true;
                }
            }
        }
        hour += 3600;
    }
    (kwh, sources)
}

/// Energy and savings for these hours; € at today's price.
fn energy(kwh: f64, asleep_h: f64, sources: Sources, power: &PowerSettings) -> Energy {
    let saved_kwh = asleep_h * (power.awake_watts - power.asleep_watts).max(0.0) / 1000.0;
    let eur = |kwh: f64| kwh * power.price_ct_per_kwh / 100.0;
    Energy {
        kwh,
        eur: eur(kwh),
        saved_kwh,
        saved_eur: eur(saved_kwh),
        source: sources.source(),
    }
}

/// Hours in each state, energy and savings for every bar from `from` to `to`
/// in the time zone `tz`, and their sum. Weeks and months are cut by the
/// calendar of `tz`; the first and the last bar count only what lies in the
/// range. `hourly` are the readings per hour, one per `step` seconds.
#[allow(clippy::too_many_arguments)]
pub fn bucket_hours(
    events: &[Event],
    hourly: &[HourSamples],
    step: i64,
    power: &PowerSettings,
    from: i64,
    to: i64,
    tz: &TimeZone,
    bucket: HistoryBucket,
) -> (Vec<HistoryDay>, HistoryTotals) {
    let spans = periods(events, from, to);
    let mut starts: Vec<Date> = Vec::new();
    for day in days(from, to, tz) {
        let start = bucket_start(day, bucket);
        if starts.last() != Some(&start) {
            starts.push(start);
        }
    }
    let mut totals = HistoryTotals::default();
    let mut all_sources = Sources::default();
    let mut total_kwh = 0.0;
    let bars = starts
        .iter()
        .filter_map(|first| {
            let start = day_start(*first, tz)?.max(from);
            let end = day_start(bucket_end(*first, bucket)?, tz)?.min(to);
            let shown = day_of(start, tz)?;
            let mut bar = HistoryDay {
                date: shown.strftime("%Y-%m-%d").to_string(),
                label: bucket_label(shown, bucket),
                awake_h: 0.0,
                gaming_h: 0.0,
                asleep_h: 0.0,
                unknown_h: 0.0,
                energy: Energy::default(),
            };
            let mut inside = Vec::new();
            for span in &spans {
                let (from, to) = (span.from.max(start), span.to.min(end));
                if to <= from {
                    continue;
                }
                let hours = (to - from) as f64 / 3600.0;
                match span.state {
                    State::Awake => bar.awake_h += hours,
                    State::Gaming => bar.gaming_h += hours,
                    State::Asleep => bar.asleep_h += hours,
                    State::Unknown => bar.unknown_h += hours,
                }
                inside.push(Period {
                    from,
                    to,
                    state: span.state,
                });
            }
            let (kwh, sources) = consumption(&inside, hourly, step, power, start, end);
            bar.energy = energy(kwh, bar.asleep_h, sources, power);
            totals.awake_h += bar.awake_h;
            totals.gaming_h += bar.gaming_h;
            totals.asleep_h += bar.asleep_h;
            totals.unknown_h += bar.unknown_h;
            total_kwh += kwh;
            all_sources.add(sources);
            Some(bar)
        })
        .collect();
    totals.energy = energy(total_kwh, totals.asleep_h, all_sources, power);
    (bars, totals)
}

/// A range, as the page asks for it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Range {
    /// The last N days, today included.
    Days(u32),
    /// The last twelve months, today included.
    Year,
    /// Back to the first event.
    All,
    /// From one day to another, both included.
    Dates(Date, Date),
}

/// Reads `7d`, `12m`, `all` or `2026-01-01..2026-03-31`; anything else is
/// the last 7 days.
fn parse_range(text: &str) -> Range {
    let text = text.trim();
    if text == "all" {
        return Range::All;
    }
    if text == "12m" {
        return Range::Year;
    }
    if let Some(days) = text.strip_suffix('d').and_then(|n| n.parse::<u32>().ok()) {
        return Range::Days(days.clamp(1, 3660));
    }
    if let Some((a, b)) = text.split_once("..")
        && let (Ok(a), Ok(b)) = (a.parse::<Date>(), b.parse::<Date>())
    {
        return Range::Dates(a.min(b), a.max(b));
    }
    Range::Days(7)
}

/// From when to when the range runs, as Unix time: from the midnight of its
/// first day to the end of its last, at most up to `now`.
fn resolve(range: Range, now: i64, first: Option<i64>, tz: &TimeZone) -> (i64, i64) {
    let Some(today) = day_of(now, tz) else {
        return (now, now);
    };
    let (first_day, last_day) = match range {
        Range::Days(days) => (
            today
                .checked_sub(i64::from(days - 1).days())
                .unwrap_or(today),
            today,
        ),
        Range::Year => (
            today
                .checked_sub(1.year())
                .and_then(|d| d.tomorrow())
                .unwrap_or(today),
            today,
        ),
        Range::All => (first.and_then(|ts| day_of(ts, tz)).unwrap_or(today), today),
        // Before 2000 there was no proxy; what comes after today has not
        // happened yet. Both keep a mistyped range small.
        Range::Dates(a, b) => (a.max(EARLIEST), b.min(today)),
    };
    let from = day_start(first_day, tz).unwrap_or(now);
    let to = last_day
        .tomorrow()
        .ok()
        .and_then(|day| day_start(day, tz))
        .unwrap_or(now)
        .min(now);
    (from.min(to), to)
}

/// What the page sees: every bar of the range and the sum.
pub async fn view(state: &Arc<AppState>, range: &str) -> HistoryView {
    let tz = clock::zone();
    let range = parse_range(range);
    // Only "all" begins at the first event.
    let first = match range {
        Range::All => state.events.first_ts().await.map(|ts| ts as i64),
        _ => None,
    };
    let (from, to) = resolve(range, clock::unix_now() as i64, first, &tz);
    // How the PC stood when the range begins is in the last event before it;
    // the others before it change nothing any more.
    let (from_u, to_u) = (from.max(0) as u64, to.max(0) as u64);
    let mut events: Vec<Event> = state
        .events
        .last_before(from_u, STATE_KINDS)
        .await
        .into_iter()
        .collect();
    events.extend(state.events.range(from_u, to_u).await);
    let cfg = state.cfg();
    let step = cfg.plug_sample.as_secs().max(1);
    let hourly = state.events.hourly(from_u, to_u, step).await;
    let power = PowerSettings {
        awake_watts: cfg.power_awake_watts,
        asleep_watts: cfg.power_asleep_watts,
        price_ct_per_kwh: cfg.price_ct_per_kwh,
    };
    let bucket = bucket_for(days(from, to, &tz).len());
    let (bars, totals) = bucket_hours(&events, &hourly, step as i64, &power, from, to, &tz, bucket);
    let date = |ts: i64| day_of(ts, &tz).map(|d| d.strftime("%Y-%m-%d").to_string());
    HistoryView {
        agent_configured: cfg.pc_agent_url.is_some(),
        power,
        days: bars,
        bucket,
        totals,
        from: date(from).unwrap_or_default(),
        // The end is no longer part of it: the last day is the one before.
        to: date(to - 1).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Zoned;

    /// Fixes the time zone so the tests do not depend on the machine's `TZ`:
    /// a day here is the one the NAS in Berlin counts.
    fn tz() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    fn ts(text: &str) -> i64 {
        text.parse::<Zoned>().unwrap().timestamp().as_second()
    }

    fn ev(kind: EventKind, at: &str) -> Event {
        Event {
            ts: ts(at) as u64,
            kind,
            detail: None,
        }
    }

    /// The hours of a day: awake, gaming, asleep, unknown.
    fn hours(day: &HistoryDay) -> (f64, f64, f64, f64) {
        (day.awake_h, day.gaming_h, day.asleep_h, day.unknown_h)
    }

    fn total(day: &HistoryDay) -> f64 {
        day.awake_h + day.gaming_h + day.asleep_h + day.unknown_h
    }

    const FROM: &str = "2026-10-18T00:00:00+02:00[Europe/Berlin]";
    const TO: &str = "2026-10-20T00:00:00+02:00[Europe/Berlin]";

    /// Two days: Sunday 18 and Monday 19 October 2026.
    fn two_days(events: &[Event]) -> Vec<HistoryDay> {
        day_hours(events, ts(FROM), ts(TO), &tz())
    }

    fn labels(days: &[HistoryDay]) -> Vec<&str> {
        days.iter().map(|d| d.date.as_str()).collect()
    }

    #[test]
    fn an_empty_history_leaves_every_hour_unknown() {
        let days = two_days(&[]);
        assert_eq!(labels(&days), ["2026-10-18", "2026-10-19"]);
        assert_eq!(days[0].label, "Sun 18 Oct");
        for day in &days {
            assert_eq!(hours(day), (0.0, 0.0, 0.0, 24.0));
        }
    }

    #[test]
    fn a_state_before_the_range_continues_into_it() {
        // The PC was asleep before the range and the proxy begin.
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-10-17T20:00:00+02:00[Europe/Berlin]",
        );
        let days = two_days(&[asleep]);
        assert_eq!(hours(&days[0]), (0.0, 0.0, 24.0, 0.0));
        assert_eq!(hours(&days[1]), (0.0, 0.0, 24.0, 0.0));
    }

    #[test]
    fn a_state_that_crosses_midnight_is_split_between_days() {
        let days = two_days(&[
            ev(
                EventKind::PcSeenAwake,
                "2026-10-18T22:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-19T02:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAwake,
                "2026-10-19T07:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[0]), (2.0, 0.0, 0.0, 22.0));
        // The awake time from Sunday runs until 2 o'clock into Monday, the
        // asleep time until 7, after that the PC is awake again.
        assert_eq!(hours(&days[1]), (19.0, 0.0, 5.0, 0.0));
        assert_eq!(total(&days[0]) + total(&days[1]), 48.0);
    }

    #[test]
    fn a_restart_leaves_unknown_from_the_last_event() {
        // What held before the restart does not hold across it: the proxy
        // may have missed something in the meantime.
        let days = two_days(&[
            ev(
                EventKind::PcSeenAwake,
                "2026-10-18T10:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::ProxyStarted,
                "2026-10-19T08:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-19T09:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[0]), (0.0, 0.0, 0.0, 24.0));
        assert_eq!(hours(&days[1]), (0.0, 0.0, 15.0, 9.0));
    }

    #[test]
    fn a_stopped_proxy_leaves_unknown_from_its_last_beat() {
        // The PC was asleep, the proxy ran until 3 o'clock and restarted at 9.
        let days = two_days(&[
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-18T00:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::ProxyStopped,
                "2026-10-19T03:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::ProxyStarted,
                "2026-10-19T09:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-19T09:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[0]), (0.0, 0.0, 24.0, 0.0));
        assert_eq!(hours(&days[1]), (0.0, 0.0, 18.0, 6.0));
    }

    #[test]
    fn a_wake_timeout_leaves_unknown_until_the_next_state() {
        // Before the failed wake-up the PC was awake.
        let days = two_days(&[
            ev(
                EventKind::PcSeenAwake,
                "2026-10-18T08:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::WakeTimeout,
                "2026-10-18T10:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-18T12:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[0]), (2.0, 0.0, 12.0, 10.0));
    }

    #[test]
    fn gaming_time_is_its_own_hours() {
        let days = two_days(&[
            ev(
                EventKind::PcSeenGaming,
                "2026-10-18T09:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::GamingOff,
                "2026-10-18T13:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[0]), (11.0, 4.0, 0.0, 9.0));
    }

    #[test]
    fn an_event_after_the_range_changes_nothing_in_it() {
        let days = two_days(&[
            ev(
                EventKind::PcSeenAwake,
                "2026-10-19T23:00:00+02:00[Europe/Berlin]",
            ),
            ev(
                EventKind::PcSeenAsleep,
                "2026-10-21T01:00:00+02:00[Europe/Berlin]",
            ),
        ]);
        assert_eq!(hours(&days[1]), (1.0, 0.0, 0.0, 23.0));
    }

    #[test]
    fn the_day_with_the_clock_back_has_25_hours() {
        let from = ts("2026-10-25T00:00:00+02:00[Europe/Berlin]");
        let to = ts("2026-10-26T00:00:00+01:00[Europe/Berlin]");
        let days = day_hours(&[], from, to, &tz());
        assert_eq!(labels(&days), ["2026-10-25"]);
        assert_eq!(total(&days[0]), 25.0);
    }

    #[test]
    fn the_day_with_the_clock_forward_has_23_hours() {
        let from = ts("2026-03-29T00:00:00+01:00[Europe/Berlin]");
        let to = ts("2026-03-30T00:00:00+02:00[Europe/Berlin]");
        let days = day_hours(&[], from, to, &tz());
        assert_eq!(labels(&days), ["2026-03-29"]);
        assert_eq!(total(&days[0]), 23.0);
    }

    #[test]
    fn a_state_over_a_clock_change_is_counted_once() {
        // In the night before 25 October 2026 the clocks go back; awake time
        // from noon Saturday to noon Monday is 12 + 25 + 12 hours.
        let awake = ev(
            EventKind::PcSeenAwake,
            "2026-10-24T12:00:00+02:00[Europe/Berlin]",
        );
        let days = day_hours(
            &[awake],
            ts("2026-10-24T12:00:00+02:00[Europe/Berlin]"),
            ts("2026-10-26T12:00:00+01:00[Europe/Berlin]"),
            &tz(),
        );
        assert_eq!(labels(&days), ["2026-10-24", "2026-10-25", "2026-10-26"]);
        assert_eq!(hours(&days[0]), (12.0, 0.0, 0.0, 0.0));
        assert_eq!(hours(&days[1]), (25.0, 0.0, 0.0, 0.0));
        assert_eq!(hours(&days[2]), (12.0, 0.0, 0.0, 0.0));
    }

    fn bucketed(events: &[Event], from: &str, to: &str, bucket: HistoryBucket) -> Vec<HistoryDay> {
        let power = PowerSettings::default();
        bucket_hours(events, &[], 60, &power, ts(from), ts(to), &tz(), bucket).0
    }

    #[test]
    fn the_bar_size_follows_the_length_of_the_range() {
        assert_eq!(bucket_for(1), HistoryBucket::Day);
        assert_eq!(bucket_for(62), HistoryBucket::Day);
        assert_eq!(bucket_for(63), HistoryBucket::Week);
        assert_eq!(bucket_for(366), HistoryBucket::Week);
        assert_eq!(bucket_for(367), HistoryBucket::Month);
    }

    #[test]
    fn weeks_start_on_monday_and_the_first_is_cut_to_the_range() {
        // Wednesday 7 October to Monday 19 October 2026, without the 19th.
        let bars = bucketed(
            &[],
            "2026-10-07T00:00:00+02:00[Europe/Berlin]",
            "2026-10-19T00:00:00+02:00[Europe/Berlin]",
            HistoryBucket::Week,
        );
        assert_eq!(labels(&bars), ["2026-10-07", "2026-10-12"]);
        assert_eq!(bars[0].label, "07 Oct");
        // Wednesday to Sunday, then a whole week.
        assert_eq!(total(&bars[0]), 5.0 * 24.0);
        assert_eq!(total(&bars[1]), 7.0 * 24.0);
    }

    #[test]
    fn a_week_with_the_clock_back_has_169_hours() {
        let bars = bucketed(
            &[],
            "2026-10-19T00:00:00+02:00[Europe/Berlin]",
            "2026-11-02T00:00:00+01:00[Europe/Berlin]",
            HistoryBucket::Week,
        );
        assert_eq!(labels(&bars), ["2026-10-19", "2026-10-26"]);
        // The clocks go back in the night to Sunday 25 October.
        assert_eq!(total(&bars[0]), 169.0);
        assert_eq!(total(&bars[1]), 168.0);
    }

    #[test]
    fn months_are_cut_at_the_first_of_each_month() {
        let awake = ev(
            EventKind::PcSeenAwake,
            "2026-01-31T12:00:00+01:00[Europe/Berlin]",
        );
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-02-01T12:00:00+01:00[Europe/Berlin]",
        );
        let bars = bucketed(
            &[awake, asleep],
            "2026-01-15T00:00:00+01:00[Europe/Berlin]",
            "2026-04-01T00:00:00+02:00[Europe/Berlin]",
            HistoryBucket::Month,
        );
        assert_eq!(labels(&bars), ["2026-01-15", "2026-02-01", "2026-03-01"]);
        assert_eq!(bars[1].label, "Feb 2026");
        // The awake time from 31 January runs 12 hours into February.
        assert_eq!(hours(&bars[0]), (12.0, 0.0, 0.0, 16.0 * 24.0 + 12.0));
        assert_eq!(hours(&bars[1]), (12.0, 0.0, 28.0 * 24.0 - 12.0, 0.0));
        // March has one hour less with the change to summer time.
        assert_eq!(hours(&bars[2]), (0.0, 0.0, 31.0 * 24.0 - 1.0, 0.0));
    }

    #[test]
    fn a_range_before_the_first_event_is_unknown_until_it() {
        let first = ev(
            EventKind::PcSeenAwake,
            "2026-10-19T06:00:00+02:00[Europe/Berlin]",
        );
        let bars = bucketed(
            &[first],
            "2026-09-01T00:00:00+02:00[Europe/Berlin]",
            "2026-10-20T00:00:00+02:00[Europe/Berlin]",
            HistoryBucket::Week,
        );
        let unknown: f64 = bars.iter().map(|b| b.unknown_h).sum();
        let awake: f64 = bars.iter().map(|b| b.awake_h).sum();
        assert_eq!(awake, 18.0);
        assert_eq!(unknown + awake, 49.0 * 24.0);
    }

    #[test]
    fn ranges_read_as_the_page_sends_them() {
        let date = |text: &str| text.parse::<Date>().unwrap();
        assert_eq!(parse_range("7d"), Range::Days(7));
        assert_eq!(parse_range("12m"), Range::Year);
        assert_eq!(parse_range("all"), Range::All);
        assert_eq!(
            parse_range("2026-03-31..2026-01-01"),
            Range::Dates(date("2026-01-01"), date("2026-03-31"))
        );
        assert_eq!(parse_range("kaputt"), Range::Days(7));
    }

    #[test]
    fn a_range_runs_from_midnight_of_its_first_day_to_now() {
        let now = ts("2026-10-19T15:00:00+02:00[Europe/Berlin]");
        let day = |text: &str| ts(&format!("{text}T00:00:00+02:00[Europe/Berlin]"));
        assert_eq!(
            resolve(Range::Days(7), now, None, &tz()),
            (day("2026-10-13"), now)
        );
        assert_eq!(resolve(Range::Year, now, None, &tz()).0, day("2025-10-20"));
        // Without an event "all" is today; otherwise from the day of the first.
        assert_eq!(resolve(Range::All, now, None, &tz()).0, day("2026-10-19"));
        let first = ts("2026-05-03T17:00:00+02:00[Europe/Berlin]");
        assert_eq!(
            resolve(Range::All, now, Some(first), &tz()).0,
            day("2026-05-03")
        );
        // A range up to a past day ends with the end of that day.
        let date = |text: &str| text.parse::<Date>().unwrap();
        assert_eq!(
            resolve(
                Range::Dates(date("2026-10-01"), date("2026-10-02")),
                now,
                None,
                &tz()
            ),
            (day("2026-10-01"), day("2026-10-03"))
        );
    }

    const POWER: PowerSettings = PowerSettings {
        awake_watts: 100.0,
        asleep_watts: 5.0,
        price_ct_per_kwh: 30.0,
    };

    /// One day, the Sunday, with readings and the settings above.
    fn one_day(events: &[Event], samples: &[HourSamples]) -> (Vec<HistoryDay>, HistoryTotals) {
        bucket_hours(
            events,
            samples,
            60,
            &POWER,
            ts(FROM),
            ts("2026-10-19T00:00:00+02:00[Europe/Berlin]"),
            &tz(),
            HistoryBucket::Day,
        )
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// A reading every minute from `from` on, for `minutes`, summed up per
    /// hour like `History::hourly`.
    fn every_minute(from: &str, minutes: i64, watts: f64) -> Vec<HourSamples> {
        let mut hours: Vec<HourSamples> = Vec::new();
        for m in 0..minutes {
            let at = ts(from) + m * 60;
            let hour = (at - at.rem_euclid(3600)) as u64;
            match hours.last_mut() {
                Some(h) if h.hour == hour => h.slots += 1,
                _ => hours.push(HourSamples {
                    hour,
                    slots: 1,
                    mean_watts: watts,
                }),
            }
        }
        hours
    }

    #[test]
    fn the_estimate_counts_gaming_as_awake_and_unknown_as_nothing() {
        // 10 h unknown, 4 h awake, 2 h gaming, 8 h asleep.
        let (days, totals) = one_day(
            &[
                ev(
                    EventKind::PcSeenAwake,
                    "2026-10-18T10:00:00+02:00[Europe/Berlin]",
                ),
                ev(
                    EventKind::PcSeenGaming,
                    "2026-10-18T14:00:00+02:00[Europe/Berlin]",
                ),
                ev(
                    EventKind::PcSeenAsleep,
                    "2026-10-18T16:00:00+02:00[Europe/Berlin]",
                ),
            ],
            &[],
        );
        let energy = days[0].energy;
        // 6 h × 100 W + 8 h × 5 W
        assert!(close(energy.kwh, 0.64), "{energy:?}");
        assert!(close(energy.eur, 0.192), "{energy:?}");
        // Only sleep saves: 8 h × (100 W − 5 W)
        assert!(close(energy.saved_kwh, 0.76), "{energy:?}");
        assert!(close(energy.saved_eur, 0.228), "{energy:?}");
        assert_eq!(energy.source, EnergySource::Estimated);
        assert_eq!(totals.energy, energy);
    }

    #[test]
    fn an_hour_half_measured_takes_the_mean_of_its_samples() {
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-10-17T00:00:00+02:00[Europe/Berlin]",
        );
        // 30 minutes at 40 W in the first hour, nothing else measured.
        let samples = every_minute(FROM, 30, 40.0);
        let (days, _) = one_day(&[asleep], &samples);
        let energy = days[0].energy;
        // 1 h × 40 W measured, 23 h × 5 W estimated
        assert!(close(energy.kwh, 0.04 + 0.115), "{energy:?}");
        assert_eq!(energy.source, EnergySource::Mixed);
        // The savings always come from the settings.
        assert!(close(energy.saved_kwh, 24.0 * 0.095), "{energy:?}");
    }

    #[test]
    fn an_hour_with_fewer_samples_stays_estimated() {
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-10-17T00:00:00+02:00[Europe/Berlin]",
        );
        let samples = every_minute(FROM, 29, 400.0);
        let (days, _) = one_day(&[asleep], &samples);
        assert!(
            close(days[0].energy.kwh, 24.0 * 0.005),
            "{:?}",
            days[0].energy
        );
        assert_eq!(days[0].energy.source, EnergySource::Estimated);
    }

    #[test]
    fn a_fully_measured_day_is_measured_even_while_unknown() {
        // No event: the state is unknown, the power plug measures.
        let samples = every_minute(FROM, 24 * 60, 50.0);
        let (days, totals) = one_day(&[], &samples);
        assert!(close(days[0].energy.kwh, 1.2), "{:?}", days[0].energy);
        assert_eq!(days[0].energy.source, EnergySource::Measured);
        assert_eq!(days[0].energy.saved_kwh, 0.0);
        assert_eq!(totals.energy.source, EnergySource::Measured);
    }

    #[test]
    fn measured_and_estimated_bars_make_mixed_totals() {
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-10-17T00:00:00+02:00[Europe/Berlin]",
        );
        // Sunday fully measured, Monday only estimated.
        let samples = every_minute(FROM, 24 * 60, 5.0);
        let (days, totals) = bucket_hours(
            &[asleep],
            &samples,
            60,
            &POWER,
            ts(FROM),
            ts(TO),
            &tz(),
            HistoryBucket::Day,
        );
        assert_eq!(days[0].energy.source, EnergySource::Measured);
        assert_eq!(days[1].energy.source, EnergySource::Estimated);
        assert_eq!(totals.energy.source, EnergySource::Mixed);
        assert!(close(totals.energy.kwh, 2.0 * 24.0 * 0.005));
    }

    #[test]
    fn a_slower_plug_measures_with_fewer_samples() {
        let asleep = ev(
            EventKind::PcSeenAsleep,
            "2026-10-17T00:00:00+02:00[Europe/Berlin]",
        );
        // A reading every 5 minutes: 6 of 12 cover half the hour.
        let hour = vec![HourSamples {
            hour: ts(FROM) as u64,
            slots: 6,
            mean_watts: 40.0,
        }];
        let (days, _) = bucket_hours(
            &[asleep],
            &hour,
            300,
            &POWER,
            ts(FROM),
            ts("2026-10-19T00:00:00+02:00[Europe/Berlin]"),
            &tz(),
            HistoryBucket::Day,
        );
        assert_eq!(days[0].energy.source, EnergySource::Mixed);
        assert!(close(days[0].energy.kwh, 0.04 + 23.0 * 0.005));
    }

    #[test]
    fn a_picked_range_stays_within_reason() {
        let now = ts("2026-10-19T15:00:00+02:00[Europe/Berlin]");
        let date = |text: &str| text.parse::<Date>().unwrap();
        let (from, to) = resolve(
            Range::Dates(date("0026-01-01"), date("9999-12-31")),
            now,
            None,
            &tz(),
        );
        assert_eq!(day_of(from, &tz()), Some(date("2000-01-01")));
        assert_eq!(to, now);
    }
}
