//! Der Verlauf: wie lange der PC an jedem Tag eines Bereichs in welchem
//! Zustand war. Die Kalenderrechnung ist rein - Ereignisse, Bereich und
//! Zeitzone hinein, die Tagestunden hinaus - damit ihre Grenzen ohne einen
//! laufenden Proxy geprueft werden koennen. Die Tage folgen der Zeitzone des
//! NAS, wie `clock::zone` sie nennt.

use std::sync::Arc;

use jiff::{
    Timestamp,
    civil::{Date, Time},
    tz::TimeZone,
};
use strata_admin_ui::api::{EventKind, HistoryDay, HistoryTotals, HistoryView};

use crate::{clock, events::Event, state::AppState};

/// Laengere Bereiche als diesen zeichnet die Weboberflaeche nicht.
pub const MAX_DAYS: u32 = 366;

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

/// Was ein Ereignis ueber den Zustand ab dann sagt; `None` bei einem, der
/// dazu nichts sagt.
fn state_after(kind: EventKind) -> Option<State> {
    match kind {
        EventKind::Awake | EventKind::PcSeenAwake | EventKind::GamingOff => Some(State::Awake),
        EventKind::SleepAccepted | EventKind::PcSeenAsleep => Some(State::Asleep),
        EventKind::PcSeenGaming | EventKind::GamingOn => Some(State::Gaming),
        _ => None,
    }
}

/// Der Tag, dem diese Zeit in `tz` gehoert.
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

/// Die Kalendertage, die der Bereich `from..to` beruehrt. Jeder faengt mit
/// seiner eigenen Mitternacht an, damit ein Tag mit Zeitumstellung 23 oder 25
/// Stunden hat; ein Tag, der erst mit `to` anfaengt, gehoert nicht dazu.
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
/// vor dem Bereich zaehlen mit: wie es zu dessen Beginn um den PC stand, steht
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
            // Ueber einen Start des Proxies reicht kein Zustand: was vor ihm
            // galt, endet schon bei dem Ereignis, das es gesetzt hat.
            None if event.kind == EventKind::ProxyStarted => state = State::Unknown,
            // Bis zum naechsten bekannten Zustand ist nichts zu beobachten.
            None if event.kind == EventKind::WakeTimeout => {
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

/// Fuegt diese Zeitspanne hinzu, gekuerzt auf den Bereich; eine, die ganz
/// auesserhalb liegt, fehlt dann.
fn add(out: &mut Vec<Period>, state: State, from: i64, to: i64, range_from: i64, range_to: i64) {
    let (from, to) = (from.max(range_from), to.min(range_to));
    if to > from {
        out.push(Period { from, to, state });
    }
}

/// Stunden in jedem Zustand fuer jeden Tag von `from` bis `to`, in der
/// Zeitzone `tz`.
pub fn day_hours(events: &[Event], from: i64, to: i64, tz: &TimeZone) -> Vec<HistoryDay> {
    let spans = periods(events, from, to);
    days(from, to, tz)
        .iter()
        .filter_map(|day| {
            let start = day_start(*day, tz)?;
            let end = day_start(day.tomorrow().ok()?, tz)?;
            let mut day_hours = HistoryDay {
                date: day.strftime("%Y-%m-%d").to_string(),
                label: day.strftime("%a %d %b").to_string(),
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
                    State::Awake => day_hours.awake_h += hours,
                    State::Gaming => day_hours.gaming_h += hours,
                    State::Asleep => day_hours.asleep_h += hours,
                    State::Unknown => day_hours.unknown_h += hours,
                }
            }
            Some(day_hours)
        })
        .collect()
}

/// Der Bereich der letzten `days` Tage, heute mitgezaehlt: von der Mitternacht
/// des ersten Tages bis jetzt.
fn range(days: u32, tz: &TimeZone) -> (i64, i64) {
    let now = clock::unix_now() as i64;
    let Some(mut first) = day_of(now, tz) else {
        return (now, now);
    };
    for _ in 1..days {
        let Some(earlier) = first.yesterday().ok() else {
            break;
        };
        first = earlier;
    }
    (day_start(first, tz).unwrap_or(now), now)
}

/// Was die Seite sieht: jeder Tag des Bereichs und die Summe.
pub async fn view(state: &Arc<AppState>, days: u32) -> HistoryView {
    let tz = clock::zone();
    let (from, to) = range(days.clamp(1, MAX_DAYS), &tz);
    // Auch die Ereignisse vor dem Bereich hoeren nicht auf zu wirken: sie
    // sagen, wie es zu seinem Beginn um den PC stand.
    let events = state
        .events
        .range(0, u64::try_from(to).unwrap_or(u64::MAX))
        .await;
    let days = day_hours(&events, from, to, &tz);
    let totals = days.iter().fold(HistoryTotals::default(), |mut sum, day| {
        sum.awake_h += day.awake_h;
        sum.gaming_h += day.gaming_h;
        sum.asleep_h += day.asleep_h;
        sum.unknown_h += day.unknown_h;
        sum
    });
    HistoryView {
        agent_configured: state.cfg().pc_agent_url.is_some(),
        days,
        totals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Zoned;

    /// Haelt die Zeitzone fest, damit die Tests nicht vom `TZ` des Rechners
    /// abhaengen: ein Tag ist hier der, den der NAS in Berlin zaehlt.
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
        // Was vor dem Neustart galt, gilt nicht ueber ihn hinweg: der Proxy
        // koennte in der Zeit etwas uebersehen haben.
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
        // In der Nacht zum 25. Oktober 2026 laufen die Uhren zurueck; wache
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
}
