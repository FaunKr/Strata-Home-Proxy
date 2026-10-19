//! Der Verlauf: wie lange der PC an jedem Tag, jeder Woche oder jedem Monat
//! eines Bereichs in welchem Zustand war. Die Kalenderrechnung ist rein -
//! Ereignisse, Bereich und Zeitzone hinein, die Stunden je Balken hinaus -
//! damit ihre Grenzen ohne einen laufenden Proxy geprüft werden können. Die
//! Tage folgen der Zeitzone des NAS, wie `clock::zone` sie nennt.

use std::sync::Arc;

use jiff::{
    Timestamp, ToSpan,
    civil::{Date, Time},
    tz::TimeZone,
};
use strata_admin_ui::api::{EventKind, HistoryBucket, HistoryDay, HistoryTotals, HistoryView};

use crate::{clock, events::Event, state::AppState};

/// Bis zu so vielen Tagen zeichnet der Verlauf einen Balken je Tag.
const DAY_BARS_UP_TO: usize = 62;
/// Bis zu so vielen Tagen einen je Woche, darüber einen je Monat.
const WEEK_BARS_UP_TO: usize = 366;

/// Die Ereignisse, die etwas darüber sagen, wie es um den PC stand.
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

/// Der Zustand des PCs, wie die Ereignisse ihn beschreiben.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Awake,
    Gaming,
    Asleep,
    Unknown,
}

/// Eine Zeitspanne, in der der PC in einem Zustand war.
struct Period {
    from: i64,
    to: i64,
    state: State,
}

/// Was ein Ereignis über den Zustand ab dann sagt; `None` bei einem, der
/// dazu nichts sagt.
fn state_after(kind: EventKind) -> Option<State> {
    match kind {
        EventKind::Awake | EventKind::PcSeenAwake | EventKind::GamingOff => Some(State::Awake),
        EventKind::SleepAccepted | EventKind::PcSeenAsleep => Some(State::Asleep),
        EventKind::PcSeenGaming | EventKind::GamingOn => Some(State::Gaming),
        _ => None,
    }
}

/// Der Tag, dem diese Zeit in `tz` gehört.
fn day_of(ts: i64, tz: &TimeZone) -> Option<Date> {
    Timestamp::from_second(ts)
        .ok()
        .map(|t| t.to_zoned(tz.clone()).date())
}

/// Der Anfang dieses Tages in `tz`; ein Tag ohne Mitternacht hat keinen.
fn day_start(day: Date, tz: &TimeZone) -> Option<i64> {
    day.to_datetime(Time::constant(0, 0, 0, 0))
        .to_zoned(tz.clone())
        .ok()
        .map(|z| z.timestamp().as_second())
}

/// Die Kalendertage, die der Bereich `from..to` berührt. Jeder fängt mit
/// seiner eigenen Mitternacht an, damit ein Tag mit Zeitumstellung 23 oder 25
/// Stunden hat; ein Tag, der erst mit `to` anfängt, gehört nicht dazu.
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

/// Die Zeitspannen eines Zustands zwischen `from` und `to`. Die Ereignisse
/// vor dem Bereich zählen mit: wie es zu dessen Beginn um den PC stand, steht
/// im letzten Ereignis davor.
fn periods(events: &[Event], from: i64, to: i64) -> Vec<Period> {
    let mut order: Vec<&Event> = events.iter().collect();
    order.sort_by_key(|event| event.ts);
    let mut out: Vec<Period> = Vec::new();
    // Zu Anfang des Bereichs hat der Proxy nichts gesehen.
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
            // Über einen Start des Proxies reicht kein Zustand. Ohne
            // `ProxyStopped` davor (ein Lauf ohne Lebenszeichen) ist nicht
            // bekannt, wann er endete: was galt, endet dann schon bei dem
            // Ereignis, das es gesetzt hat.
            None if event.kind == EventKind::ProxyStarted => state = State::Unknown,
            // Bis zum nächsten bekannten Zustand ist nichts zu beobachten:
            // nach einem Wecken ohne Antwort, und ab dem letzten Lebenszeichen
            // eines beendeten Proxies.
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

/// Fügt diese Zeitspanne hinzu, gekürzt auf den Bereich; eine, die ganz
/// außerhalb liegt, fehlt dann.
fn add(out: &mut Vec<Period>, state: State, from: i64, to: i64, range_from: i64, range_to: i64) {
    let (from, to) = (from.max(range_from), to.min(range_to));
    if to > from {
        out.push(Period { from, to, state });
    }
}

/// Die Größe der Balken für einen Bereich über so viele Tage.
pub fn bucket_for(days: usize) -> HistoryBucket {
    if days <= DAY_BARS_UP_TO {
        HistoryBucket::Day
    } else if days <= WEEK_BARS_UP_TO {
        HistoryBucket::Week
    } else {
        HistoryBucket::Month
    }
}

/// Der erste Tag des Balkens, zu dem `day` gehört; Wochen fangen montags an.
fn bucket_start(day: Date, bucket: HistoryBucket) -> Date {
    match bucket {
        HistoryBucket::Day => day,
        HistoryBucket::Week => day
            .checked_sub(i64::from(day.weekday().to_monday_zero_offset()).days())
            .unwrap_or(day),
        HistoryBucket::Month => day.first_of_month(),
    }
}

/// Der erste Tag des Balkens danach.
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

/// Stunden in jedem Zustand für jeden Tag von `from` bis `to`, in der
/// Zeitzone `tz`.
pub fn day_hours(events: &[Event], from: i64, to: i64, tz: &TimeZone) -> Vec<HistoryDay> {
    bucket_hours(events, from, to, tz, HistoryBucket::Day)
}

/// Stunden in jedem Zustand für jeden Balken von `from` bis `to`, in der
/// Zeitzone `tz`. Wochen und Monate schneidet der Kalender von `tz`; der
/// erste und der letzte Balken zählen nur, was im Bereich liegt.
pub fn bucket_hours(
    events: &[Event],
    from: i64,
    to: i64,
    tz: &TimeZone,
    bucket: HistoryBucket,
) -> Vec<HistoryDay> {
    let spans = periods(events, from, to);
    let mut starts: Vec<Date> = Vec::new();
    for day in days(from, to, tz) {
        let start = bucket_start(day, bucket);
        if starts.last() != Some(&start) {
            starts.push(start);
        }
    }
    starts
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
            };
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
            }
            Some(bar)
        })
        .collect()
}

/// Ein Bereich, wie die Seite ihn verlangt.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Range {
    /// Die letzten so vielen Tage, heute mitgezählt.
    Days(u32),
    /// Die letzten zwölf Monate, heute mitgezählt.
    Year,
    /// Zurück bis zum ersten Ereignis.
    All,
    /// Von einem Tag bis zu einem anderen, beide mitgezählt.
    Dates(Date, Date),
}

/// Liest `7d`, `12m`, `all` oder `2026-01-01..2026-03-31`; alles andere sind
/// die letzten 7 Tage.
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

/// Von wann bis wann der Bereich reicht, als Unix-Zeit: von der Mitternacht
/// seines ersten Tages bis zum Ende seines letzten, höchstens bis `now`.
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
        Range::Dates(a, b) => (a, b),
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

/// Was die Seite sieht: jeder Balken des Bereichs und die Summe.
pub async fn view(state: &Arc<AppState>, range: &str) -> HistoryView {
    let tz = clock::zone();
    let first = state.events.first_ts().await.map(|ts| ts as i64);
    let (from, to) = resolve(parse_range(range), clock::unix_now() as i64, first, &tz);
    // Wie es zu Beginn des Bereichs um den PC stand, sagt das letzte Ereignis
    // davor; die anderen davor ändern nichts mehr.
    let (from_u, to_u) = (from.max(0) as u64, to.max(0) as u64);
    let mut events: Vec<Event> = state
        .events
        .last_before(from_u, STATE_KINDS)
        .await
        .into_iter()
        .collect();
    events.extend(state.events.range(from_u, to_u).await);
    let day_count = days(from, to, &tz).len();
    let bucket = bucket_for(day_count);
    let bars = bucket_hours(&events, from, to, &tz, bucket);
    let totals = bars.iter().fold(HistoryTotals::default(), |mut sum, bar| {
        sum.awake_h += bar.awake_h;
        sum.gaming_h += bar.gaming_h;
        sum.asleep_h += bar.asleep_h;
        sum.unknown_h += bar.unknown_h;
        sum
    });
    let date = |ts: i64| day_of(ts, &tz).map(|d| d.strftime("%Y-%m-%d").to_string());
    HistoryView {
        agent_configured: state.cfg().pc_agent_url.is_some(),
        days: bars,
        bucket,
        totals,
        from: date(from).unwrap_or_default(),
        // Das Ende gehört nicht mehr dazu: der letzte Tag ist der davor.
        to: date(to - 1).unwrap_or_default(),
        first_day: first.and_then(date),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Zoned;

    /// Hält die Zeitzone fest, damit die Tests nicht vom `TZ` des Rechners
    /// abhängen: ein Tag ist hier der, den der NAS in Berlin zählt.
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

    /// Die Stunden eines Tages: wach, gaming, schlafend, unbekannt.
    fn hours(day: &HistoryDay) -> (f64, f64, f64, f64) {
        (day.awake_h, day.gaming_h, day.asleep_h, day.unknown_h)
    }

    fn total(day: &HistoryDay) -> f64 {
        day.awake_h + day.gaming_h + day.asleep_h + day.unknown_h
    }

    const FROM: &str = "2026-10-18T00:00:00+02:00[Europe/Berlin]";
    const TO: &str = "2026-10-20T00:00:00+02:00[Europe/Berlin]";

    /// Zwei Tage: Sonntag der 18. und Montag der 19. Oktober 2026.
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
        // Der PC schlief schon, bevor der Bereich und der Proxy anfangen.
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
        // Die wache Zeit vom Sonntag reicht bis 2 Uhr in den Montag, die
        // schlafende bis 7 Uhr, danach ist der PC wieder wach.
        assert_eq!(hours(&days[1]), (19.0, 0.0, 5.0, 0.0));
        assert_eq!(total(&days[0]) + total(&days[1]), 48.0);
    }

    #[test]
    fn a_restart_leaves_unknown_from_the_last_event() {
        // Was vor dem Neustart galt, gilt nicht über ihn hinweg: der Proxy
        // könnte in der Zeit etwas übersehen haben.
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
        // Der PC schlief, der Proxy lief bis 3 Uhr und startete um 9 Uhr neu.
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
        // Vor dem fehlgeschlagenen Aufwachen war der PC wach.
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
        // In der Nacht zum 25. Oktober 2026 laufen die Uhren zurück; wache
        // Zeit von Mittag Sonnabend bis Mittag Montag sind 12 + 25 + 12 Stunden.
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
        bucket_hours(events, ts(from), ts(to), &tz(), bucket)
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
        // Mittwoch, 7. Oktober, bis Montag, 19. Oktober 2026, ohne den 19.
        let bars = bucketed(
            &[],
            "2026-10-07T00:00:00+02:00[Europe/Berlin]",
            "2026-10-19T00:00:00+02:00[Europe/Berlin]",
            HistoryBucket::Week,
        );
        assert_eq!(labels(&bars), ["2026-10-07", "2026-10-12"]);
        assert_eq!(bars[0].label, "07 Oct");
        // Mittwoch bis Sonntag, dann eine ganze Woche.
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
        // Die Uhren laufen in der Nacht zum Sonntag, 25. Oktober, zurück.
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
        // Die wache Zeit vom 31. Januar reicht 12 Stunden in den Februar.
        assert_eq!(hours(&bars[0]), (12.0, 0.0, 0.0, 16.0 * 24.0 + 12.0));
        assert_eq!(hours(&bars[1]), (12.0, 0.0, 28.0 * 24.0 - 12.0, 0.0));
        // Der März hat mit der Umstellung auf Sommerzeit eine Stunde weniger.
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
        // Ohne Ereignis ist "alles" heute; sonst ab dem Tag des ersten.
        assert_eq!(resolve(Range::All, now, None, &tz()).0, day("2026-10-19"));
        let first = ts("2026-05-03T17:00:00+02:00[Europe/Berlin]");
        assert_eq!(
            resolve(Range::All, now, Some(first), &tz()).0,
            day("2026-05-03")
        );
        // Ein Bereich bis zu einem vergangenen Tag endet mit dessen Ende.
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
}
