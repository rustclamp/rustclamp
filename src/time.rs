//! UTC dates and times on [`SystemTime`], without a time crate: RFC 3339
//! text, calendar dates and day arithmetic, and local time in a [`Zone`]
//! with a fixed offset or EU summer time.
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

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// The UTC hour (0 to 23) and minute (0 to 59) of `time`.
pub fn hour_minute(time: SystemTime) -> (u32, u32) {
    let of_day = seconds(time).rem_euclid(DAY) as u32;
    (of_day / 3600, of_day % 3600 / 60)
}

/// The UTC day of the week of `time`.
pub fn weekday(time: SystemTime) -> Weekday {
    use Weekday::{Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday};
    // 1970-01-01 was a Thursday.
    [
        Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday,
    ][(seconds(time).div_euclid(DAY) + 3).rem_euclid(7) as usize]
}

/// A time zone without a tz database: a fixed offset from UTC, or a
/// standard offset with EU summer time. Summer time is one hour ahead, from
/// 01:00 UTC on the last Sunday of March to 01:00 UTC on the last Sunday of
/// October, the same instant in every EU zone (Directive 2000/84/EC).
///
/// ```
/// use rustclamp::time::{self, Zone};
///
/// let central = Zone::eu(60); // CET/CEST: Ljubljana, Berlin, Paris
/// let at = time::parse_rfc3339("2026-10-20T22:30:00Z").unwrap();
/// assert_eq!(central.format_date(at), "2026-10-21");
/// assert_eq!(central.format_rfc3339(at), "2026-10-21T00:30:00+02:00");
/// assert_eq!(time::format_rfc3339(central.start_of_day(at)), "2026-10-20T22:00:00Z");
/// ```
// ponytail: EU rule only; other regions' rules or a tz database when an app needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    /// Minutes east of UTC outside summer time.
    standard: i32,
    eu_summer_time: bool,
}

impl Zone {
    /// UTC.
    pub const UTC: Self = Self::fixed(0);

    /// Always `minutes` east of UTC (negative for west): `Zone::fixed(-300)`
    /// is UTC-5 all year.
    ///
    /// # Panics
    ///
    /// When `minutes` is a day or more.
    pub const fn fixed(minutes: i32) -> Self {
        assert!(minutes.abs() < 24 * 60, "a zone offset is under a day");
        Self {
            standard: minutes,
            eu_summer_time: false,
        }
    }

    /// `standard_minutes` east of UTC, an hour more in EU summer time:
    /// `Zone::eu(0)` for WET/WEST, `eu(60)` for CET/CEST, `eu(120)` for EET/EEST.
    ///
    /// # Panics
    ///
    /// When `standard_minutes` is a day or more.
    pub const fn eu(standard_minutes: i32) -> Self {
        Self {
            eu_summer_time: true,
            ..Self::fixed(standard_minutes)
        }
    }

    /// Minutes east of UTC at `at`.
    pub fn offset_at(self, at: SystemTime) -> i32 {
        let now = seconds(at);
        let year = split(now).0[..4].parse().unwrap_or(1970);
        let summer = self.eu_summer_time
            && (last_sunday_one_utc(year, 3)..last_sunday_one_utc(year, 10)).contains(&now);
        self.standard + if summer { 60 } else { 0 }
    }

    /// The local calendar date of `at`, `YYYY-MM-DD`.
    pub fn format_date(self, at: SystemTime) -> String {
        split(self.local_seconds(at)).0
    }

    /// `at` as RFC 3339 in local time with its offset, whole seconds:
    /// `2026-10-21T00:30:00+02:00`; `Z` when the offset is 0.
    pub fn format_rfc3339(self, at: SystemTime) -> String {
        let (date, clock) = split(self.local_seconds(at));
        let offset = self.offset_at(at);
        if offset == 0 {
            return format!("{date}T{clock}Z");
        }
        let sign = if offset < 0 { '-' } else { '+' };
        let offset = offset.unsigned_abs();
        format!("{date}T{clock}{sign}{:02}:{:02}", offset / 60, offset % 60)
    }

    /// The local hour (0 to 23) and minute (0 to 59) of `at`.
    pub fn hour_minute(self, at: SystemTime) -> (u32, u32) {
        let of_day = self.local_seconds(at).rem_euclid(DAY) as u32;
        (of_day / 3600, of_day % 3600 / 60)
    }

    /// The local day of the week of `at`.
    pub fn weekday(self, at: SystemTime) -> Weekday {
        weekday(from_seconds(self.local_seconds(at)))
    }

    /// The instant the local day of `at` began, local midnight: the lower
    /// bound for "today" in a query.
    pub fn start_of_day(self, at: SystemTime) -> SystemTime {
        let midnight = self.local_seconds(at).div_euclid(DAY) * DAY;
        // Midnight's own offset, which differs from `at`'s on a switch day.
        let guess = from_seconds(midnight - i64::from(self.offset_at(at)) * 60);
        from_seconds(midnight - i64::from(self.offset_at(guess)) * 60)
    }

    fn local_seconds(self, at: SystemTime) -> i64 {
        seconds(at) + i64::from(self.offset_at(at)) * 60
    }
}

/// Unix seconds of 01:00 UTC on the last Sunday of `month` (March or
/// October, both 31 days long) of `year`.
fn last_sunday_one_utc(year: i64, month: u32) -> i64 {
    let last = days(&format!("{year:04}-{month:02}-31")).unwrap_or(0);
    // 1970-01-01, day 0, was a Thursday: (day + 3) % 7 is 0 on a Monday.
    let since_sunday = (last + 4).rem_euclid(7);
    (last - since_sunday) * DAY + 3600
}

/// `YYYY-MM-DD HH:MM:SS` in UTC for Unix `seconds`, the form SQLite's
/// `CURRENT_TIMESTAMP` writes.
#[cfg(any(feature = "log", feature = "db-core"))]
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
    fn hours_minutes_and_weekdays_are_utc() {
        let at = parse_rfc3339("2026-10-07T03:05:59+02:00").unwrap();
        assert_eq!(hour_minute(at), (1, 5));
        assert_eq!(weekday(at), Weekday::Wednesday);
        assert_eq!(weekday(UNIX_EPOCH), Weekday::Thursday);
        assert_eq!(
            weekday(UNIX_EPOCH - Duration::from_secs(1)),
            Weekday::Wednesday
        );
        assert_eq!(hour_minute(UNIX_EPOCH - Duration::from_secs(1)), (23, 59));
    }

    #[test]
    #[cfg(any(feature = "log", feature = "db-core"))]
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
    fn eu_summer_time_switches_at_one_utc_on_the_last_sundays() {
        let at = |text| parse_rfc3339(text).unwrap();
        let central = Zone::eu(60);
        for (text, offset) in [
            ("2026-03-29T00:59:59Z", 60),
            ("2026-03-29T01:00:00Z", 120),
            ("2026-10-25T00:59:59Z", 120),
            ("2026-10-25T01:00:00Z", 60),
            ("2025-03-30T01:00:00Z", 120),
            ("2025-03-29T12:00:00Z", 60),
            ("2026-01-01T00:00:00Z", 60),
        ] {
            assert_eq!(central.offset_at(at(text)), offset, "{text}");
        }
        assert_eq!(Zone::eu(0).offset_at(at("2026-07-01T00:00:00Z")), 60);
        assert_eq!(Zone::fixed(60).offset_at(at("2026-07-01T00:00:00Z")), 60);
        assert_eq!(Zone::UTC.offset_at(at("2026-07-01T00:00:00Z")), 0);
    }

    #[test]
    fn local_dates_clocks_and_midnights() {
        let at = |text| parse_rfc3339(text).unwrap();
        let central = Zone::eu(60);
        let late = at("2026-12-31T23:30:00Z");
        assert_eq!(central.format_date(late), "2027-01-01");
        assert_eq!(central.format_rfc3339(late), "2027-01-01T00:30:00+01:00");
        assert_eq!(central.hour_minute(late), (0, 30));
        assert_eq!(central.weekday(late), Weekday::Friday);
        assert_eq!(format_date(late), "2026-12-31");
        // The day summer time starts began at +01:00, though noon is at +02:00.
        let switch = at("2026-03-29T12:00:00Z");
        assert_eq!(
            format_rfc3339(central.start_of_day(switch)),
            "2026-03-28T23:00:00Z"
        );
        assert_eq!(
            format_rfc3339(central.start_of_day(at("2026-10-25T12:00:00Z"))),
            "2026-10-24T22:00:00Z"
        );
        let west = Zone::fixed(-330);
        assert_eq!(
            west.format_rfc3339(at("2026-01-01T03:00:00Z")),
            "2025-12-31T21:30:00-05:30"
        );
        assert_eq!(Zone::UTC.format_rfc3339(late), format_rfc3339(late));
        assert_eq!(
            format_rfc3339(west.start_of_day(late)),
            "2026-12-31T05:30:00Z"
        );
    }

    #[test]
    #[should_panic(expected = "under a day")]
    fn offsets_are_under_a_day() {
        let _ = Zone::fixed(24 * 60);
    }

    #[test]
    fn days_move_across_months_and_years() {
        let new_year = parse_date("2024-12-31").unwrap();
        assert_eq!(format_date(add_days(new_year, 1)), "2025-01-01");
        assert_eq!(format_date(add_days(new_year, -306)), "2024-02-29");
        assert_eq!(format_date(add_days(UNIX_EPOCH, -1)), "1969-12-31");
    }
}
