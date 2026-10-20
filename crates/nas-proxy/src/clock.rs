//! Wall-clock times for the overview, in the NAS's time zone (`TZ`).

use std::time::{SystemTime, UNIX_EPOCH};

use jiff::{Timestamp, Zoned, civil::Time, tz::TimeZone};

/// Unix time in seconds.
pub fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn unix_now() -> u64 {
    unix(SystemTime::now())
}

/// The time zone of the NAS: the days of the history follow it, so the same
/// range reads the same on the NAS and on any page.
pub fn zone() -> TimeZone {
    TimeZone::system()
}

fn zoned(ts: u64) -> Zoned {
    let ts = Timestamp::from_second(ts as i64).unwrap_or(Timestamp::UNIX_EPOCH);
    ts.to_zoned(TimeZone::system())
}

/// `17.10.2026`.
pub fn local_date(ts: u64) -> String {
    zoned(ts).strftime("%d.%m.%Y").to_string()
}

/// `14:03` for today, `17.10. 14:03` for other days.
pub fn local_text(ts: u64) -> String {
    let at = zoned(ts);
    let today = zoned(unix_now()).date();
    if at.date() == today {
        at.strftime("%H:%M").to_string()
    } else {
        at.strftime("%d.%m. %H:%M").to_string()
    }
}

/// The next 8 o'clock after `now` in its time zone: today if it is still
/// before 8, else tomorrow.
pub fn next_morning(now: &Zoned) -> Timestamp {
    let eight = Time::constant(8, 0, 0, 0);
    let today = now.date().to_datetime(eight);
    let day = if now.datetime() < today {
        now.date()
    } else {
        now.date().tomorrow().unwrap_or(now.date())
    };
    day.to_datetime(eight)
        .to_zoned(now.time_zone().clone())
        .map(|z| z.timestamp())
        .unwrap_or(now.timestamp())
}

/// `next_morning` from now, in the NAS's time zone, as Unix seconds.
pub fn next_morning_unix() -> u64 {
    next_morning(&Zoned::now()).as_second().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    fn morning(text: &str) -> String {
        let now = at(text);
        next_morning(&now)
            .to_zoned(now.time_zone().clone())
            .to_string()
    }

    #[test]
    fn before_eight_means_today() {
        assert_eq!(
            morning("2026-10-18T06:30:00+02:00[Europe/Berlin]"),
            "2026-10-18T08:00:00+02:00[Europe/Berlin]"
        );
    }

    #[test]
    fn at_or_after_eight_means_tomorrow() {
        assert_eq!(
            morning("2026-10-18T08:00:00+02:00[Europe/Berlin]"),
            "2026-10-19T08:00:00+02:00[Europe/Berlin]"
        );
        assert_eq!(
            morning("2026-10-18T23:10:00+02:00[Europe/Berlin]"),
            "2026-10-19T08:00:00+02:00[Europe/Berlin]"
        );
    }

    #[test]
    fn follows_the_clock_change() {
        // Summer time ends in the night before 25 October 2026: 11 hours, not 10.
        let now = at("2026-10-24T22:00:00+02:00[Europe/Berlin]");
        let target = next_morning(&now);
        assert_eq!(
            target.to_zoned(now.time_zone().clone()).to_string(),
            "2026-10-25T08:00:00+01:00[Europe/Berlin]"
        );
        assert_eq!(target.as_second() - now.timestamp().as_second(), 11 * 3600);
    }

    #[test]
    fn local_text_shows_the_date_for_other_days() {
        assert_eq!(local_text(unix_now()).len(), 5);
        assert!(local_text(unix_now() - 3 * 86_400).contains('.'));
    }
}
