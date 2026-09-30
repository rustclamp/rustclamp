//! SQLite `CURRENT_TIMESTAMP` values (`YYYY-MM-DD HH:MM:SS`, UTC) for display.

/// `time` written the way `CURRENT_TIMESTAMP` writes it, so rows stamped by
/// the app's clock and by SQLite sort together; times before 1970 are 1970.
///
/// ```
/// use std::time::{Duration, UNIX_EPOCH};
///
/// let time = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
/// assert_eq!(rustclamp::db::timestamp::format(time), "2001-09-09 01:46:40");
/// ```
pub fn format(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    crate::time::timestamp(seconds)
}

/// The day, `YYYY-MM-DD`; a value too short to hold one is returned as-is.
///
/// ```
/// assert_eq!(rustclamp::db::timestamp::date("2026-09-30 08:15:00"), "2026-09-30");
/// ```
pub fn date(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

/// ISO 8601 in UTC, as a `<time datetime>` wants: `2026-09-30T08:15:00Z`.
///
/// ```
/// assert_eq!(rustclamp::db::timestamp::iso8601("2026-09-30 08:15:00"), "2026-09-30T08:15:00Z");
/// ```
pub fn iso8601(timestamp: &str) -> String {
    format!("{}Z", timestamp.replacen(' ', "T", 1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_values_do_not_panic() {
        assert_eq!(super::date("2026"), "2026");
        assert_eq!(super::date(""), "");
    }
}
