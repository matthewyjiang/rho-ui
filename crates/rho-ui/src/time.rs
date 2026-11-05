//! Wall-clock helpers: Unix seconds, RFC 3339 parsing, and compact ages.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_secs()).ok())
        .unwrap_or(0)
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

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as
/// `2026-11-04T12:34:56.789Z` or `2026-11-04T07:34:56-05:00`.
pub(crate) fn unix_seconds(stamp: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    // Skip fractional seconds, then read the zone.
    let zone = stamp
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let hours = zone.get(1..3)?.parse::<i64>().ok()?;
            let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
            let offset = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                offset
            } else {
                -offset
            }
        }
        _ => return None,
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_with_zones_and_fractions() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            unix_seconds("2026-11-04T12:00:00.123456Z"),
            Some(1_793_793_600)
        );
        assert_eq!(
            unix_seconds("2026-11-04T07:00:00-05:00"),
            Some(1_793_793_600)
        );
        assert_eq!(unix_seconds("yesterday"), None);
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
