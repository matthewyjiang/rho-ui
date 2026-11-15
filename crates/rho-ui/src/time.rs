//! Wall-clock helpers: Unix seconds, RFC 3339 parsing, compact ages and
//! turn lengths.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_secs()).ok())
        .unwrap_or(0)
}

/// Unix seconds of a UTC RFC 3339 time as GitHub writes it,
/// `2026-11-15T05:11:35Z`. `None` for any other form.
pub(crate) fn parse_utc(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = time.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from 1970-01-01 to the date, by Howard Hinnant's
    // `days_from_civil`: years start in March, so a leap day ends one.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Compact age for `seconds` ago: "now", "5m", "3h", "2d", "6w", "1y".
pub(crate) fn relative_age(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;
    match seconds.max(0) {
        s if s < MINUTE => "now".to_owned(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < WEEK => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}w", s / WEEK),
        s => format!("{}y", s / YEAR),
    }
}

/// A running duration in whole minutes: "4m", "1h 5m". `None` under a
/// minute, so a label that redraws every 30 seconds is never stale.
pub(crate) fn elapsed(seconds: u64) -> Option<String> {
    match seconds {
        s if s < 60 => None,
        s if s < 3600 => Some(format!("{}m", s / 60)),
        s => Some(format!("{}h {}m", s / 3600, s % 3600 / 60)),
    }
}

/// How long a turn took, as T3 Code writes it: "850ms", "4.2s", "38s",
/// "1m 20s", "2h 5m".
pub(crate) fn took(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        return format!("{}ms", millis.max(1));
    }
    if millis < 10_000 {
        // Tenths of a second, rounded half up: 4_250 ms is "4.3s".
        let tenths = (millis + 50) / 100;
        return format!("{}.{}s", tenths / 10, tenths % 10);
    }
    // Round before choosing units, so 59.5 s never reads "60s".
    hms((millis + 500) / 1_000)
}

/// How long a running turn has run, in whole seconds so it ticks once a
/// second: "0s", "38s", "1m 20s". It reads as [`took`] from 10 s on.
pub(crate) fn running(duration: Duration) -> String {
    hms(u128::from(duration.as_secs()))
}

/// "38s", "1m 20s", "2h 5m": units with nothing in them are left out.
fn hms(seconds: u128) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let parts = [
        (seconds / 3_600, "h"),
        (seconds % 3_600 / 60, "m"),
        (seconds % 60, "s"),
    ];
    parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| format!("{count}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_lengths_read_like_t3_code() {
        let ms = Duration::from_millis;
        assert_eq!(took(ms(0)), "1ms");
        assert_eq!(took(ms(4_250)), "4.3s");
        assert_eq!(took(ms(59_400)), "59s");
        assert_eq!(took(ms(59_600)), "1m");
        assert_eq!(took(ms(80_000)), "1m 20s");
        assert_eq!(took(ms(7_500_000)), "2h 5m");
    }

    /// GitHub's times read as the Unix seconds `date -u` gives, leap days
    /// and the epoch's edge included; other forms read as none.
    #[test]
    fn utc_times_parse() {
        let cases = [
            ("1970-01-01T00:00:00Z", Some(0)),
            ("1969-12-31T23:59:59Z", Some(-1)),
            ("2000-02-29T12:00:00Z", Some(951_825_600)),
            ("2026-11-15T05:11:35Z", Some(1_794_719_495)),
            ("2026-11-15T05:11:35+01:00", None),
            ("2026-13-01T00:00:00Z", None),
            ("soon", None),
        ];
        for (text, unix) in cases {
            assert_eq!(parse_utc(text), unix, "{text}");
        }
    }

    #[test]
    fn a_running_turn_ticks_in_whole_seconds() {
        let ms = Duration::from_millis;
        assert_eq!(running(ms(400)), "0s");
        assert_eq!(running(ms(4_900)), "4s");
        assert_eq!(running(ms(80_999)), "1m 20s");
    }

    #[test]
    fn ages_and_durations_are_compact() {
        assert_eq!(relative_age(5), "now");
        assert_eq!(relative_age(5 * 60), "5m");
        assert_eq!(relative_age(3 * 3600), "3h");
        assert_eq!(relative_age(15 * 86_400), "2w");
        assert_eq!(elapsed(12), None);
        assert_eq!(elapsed(4 * 60 + 3).as_deref(), Some("4m"));
        assert_eq!(elapsed(3900).as_deref(), Some("1h 5m"));
    }
}
