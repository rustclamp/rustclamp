//! SQLite `CURRENT_TIMESTAMP` values (`YYYY-MM-DD HH:MM:SS`, UTC) for display.

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
