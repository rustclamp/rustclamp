//! UTC dates and times on [`SystemTime`], without a time crate: RFC 3339
//! text, calendar dates and day arithmetic.
//!
//! Enabled by the `time` feature (`log` and `db` include it).
//!
//! ```
//! use rustclamp::time;
//!
//! let at = time::parse_rfc3339("2026-09-30T08:15:00+02:00").unwrap();
//! assert_eq!(time::format_rfc3339(at), "2026-09-30T06:15:00Z");
//! let due = time::add_days(at, 30);
//! assert_eq!(time::format_date(due), "2026-10-30");
//! assert_eq!(time::parse_date("2026-10-30"), time::parse_rfc3339("2026-10-30T00:00:00Z"));
//! ```

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;

/// `time` as RFC 3339 in UTC, whole seconds: `2026-09-30T08:15:00Z`.
pub fn format_rfc3339(time: SystemTime) -> String {
    let (date, clock) = split(seconds(time));
    format!("{date}T{clock}Z")
}

/// The UTC calendar date of `time`, `YYYY-MM-DD`.
pub fn format_date(time: SystemTime) -> String {
    split(seconds(time)).0
}

/// Reads RFC 3339, such as `2026-09-30T08:15:00Z`, `…T08:15:00.25+02:00`,
/// or with a space for the `T`. `None` when it is not a valid date and time.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let (date, rest) = (text.get(..10)?, text.get(10..)?);
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (hour, minute, second) = (
        number(rest.get(..2)?, 0, 23)?,
        number(rest.get(3..5)?, 0, 59)?,
        number(rest.get(6..8)?, 0, 60)?,
    );
    if rest.get(2..3)? != ":" || rest.get(5..6)? != ":" {
        return None;
    }
    let mut rest = &rest[8..];
    let mut nanos = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<9}", &fraction[..digits.min(9)]);
        nanos = padded.parse().ok()?;
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.get(3..4)? != ":" {
                return None;
            }
            sign * (number(rest.get(1..3)?, 0, 23)? * 3600 + number(rest.get(4..6)?, 0, 59)? * 60)
        }
    };
    // A leap second (:60) is read as the next second, as most systems do.
    let at = days(date)? * DAY + hour * 3600 + minute * 60 + second - offset;
    Some(from_seconds(at) + Duration::from_nanos(nanos))
}

/// Midnight UTC on the date `YYYY-MM-DD`; `None` when it is not a real date.
pub fn parse_date(text: &str) -> Option<SystemTime> {
    Some(from_seconds(days(text)? * DAY))
}

/// `time` moved by whole days, back when `days` is negative.
pub fn add_days(time: SystemTime, days: i64) -> SystemTime {
    let shift = Duration::from_secs(days.unsigned_abs() * DAY as u64);
    if days < 0 { time - shift } else { time + shift }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC for Unix `seconds`, the form SQLite's
/// `CURRENT_TIMESTAMP` writes.
#[cfg(any(feature = "log", feature = "db"))]
pub(crate) fn timestamp(seconds: u64) -> String {
    let (date, clock) = split(seconds as i64);
    format!("{date} {clock}")
}

/// Whole seconds since the Unix epoch, negative before it.
fn seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        Err(before) => -(before.duration().as_secs_f64().ceil() as i64),
    }
}

fn from_seconds(seconds: i64) -> SystemTime {
    let span = Duration::from_secs(seconds.unsigned_abs());
    if seconds < 0 {
        UNIX_EPOCH - span
    } else {
        UNIX_EPOCH + span
    }
}

/// `seconds` as a `YYYY-MM-DD` date and an `HH:MM:SS` time.
fn split(seconds: i64) -> (String, String) {
    let (days, time) = (seconds.div_euclid(DAY), seconds.rem_euclid(DAY));
    // Days to a civil date: Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!(
            "{:02}:{:02}:{:02}",
            time / 3600,
            time % 3600 / 60,
            time % 60
        ),
    )
}

/// Days since the Unix epoch for `YYYY-MM-DD`: Hinnant's `days_from_civil`.
fn days(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let (year, month) = (number(&date[..4], 0, 9999)?, number(&date[5..7], 1, 12)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let length = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let day = number(&date[8..], 1, length[month as usize - 1])?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// ASCII digits in `min..=max`.
fn number(text: &str, min: i64, max: i64) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|n| (min..=max).contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(feature = "log", feature = "db"))]
    fn timestamps_are_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(timestamp(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(timestamp(1_790_693_195), "2026-09-29 14:46:35");
        assert_eq!(timestamp(4_107_542_399), "2100-02-28 23:59:59");
    }

    #[test]
    fn rfc3339_round_trips_across_calendar_edges() {
        for text in [
            "1970-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "1900-03-01T12:00:00Z",
            "2000-02-29T00:00:00Z",
            "2026-09-30T08:15:00Z",
            "2100-02-28T23:59:59Z",
        ] {
            assert_eq!(format_rfc3339(parse_rfc3339(text).unwrap()), text);
        }
        let at = parse_rfc3339("2026-09-30T08:15:00.250-01:30").unwrap();
        assert_eq!(format_rfc3339(at), "2026-09-30T09:45:00Z");
        assert_eq!(at.duration_since(UNIX_EPOCH).unwrap().subsec_millis(), 250);
        assert_eq!(
            parse_rfc3339("2026-09-30 08:15:00z"),
            parse_rfc3339("2026-09-30T08:15:00Z")
        );
    }

    #[test]
    fn invalid_text_is_none() {
        for text in [
            "",
            "2026-09-30",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-09-30T24:00:00Z",
            "2026-09-30T08:15:00",
            "2026-09-30T08:15:00+0200",
            "2026-09-30T08:15:00.Z",
            "2026-09-30T08-15-00Z",
            "+026-09-30T08:15:00Z",
        ] {
            assert_eq!(parse_rfc3339(text), None, "{text}");
        }
        assert_eq!(parse_date("2024-02-30"), None);
        assert_eq!(parse_date("2024-2-3"), None);
    }

    #[test]
    fn days_move_across_months_and_years() {
        let new_year = parse_date("2024-12-31").unwrap();
        assert_eq!(format_date(add_days(new_year, 1)), "2025-01-01");
        assert_eq!(format_date(add_days(new_year, -306)), "2024-02-29");
        assert_eq!(format_date(add_days(UNIX_EPOCH, -1)), "1969-12-31");
    }
}
