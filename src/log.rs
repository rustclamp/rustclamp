//! Application logging with the PSR-3 levels Laravel uses.
//!
//! Enabled by the `log` feature (the `web` feature includes it).
//!
//! ```no_run
//! use rustclamp::log::Log;
//!
//! Log::info("server started");
//! Log::warning(format_args!("slow request: {} ms", 850));
//! ```
//!
//! Settings come from `.env` and the environment ([`Config::load`]), read on
//! the first log call:
//!
//! - `LOG_LEVEL`: the lowest level written, `debug` by default. `silent`
//!   turns logging off.
//! - `LOG_CHANNEL`: `file` (default) appends to `storage/logs/app.log`;
//!   `stderr` writes to standard error, for systemd or containers.
//! - `APP_ENV`: the environment name in each line, `local` by default.
//!
//! Each entry is one line, `[2026-09-29 14:03:07] local.INFO: message`, with
//! line breaks in the message escaped so one entry never spans lines. Times
//! are UTC.

use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;

/// The log file used by the `file` channel.
pub const FILE: &str = "storage/logs/app.log";

/// Severity, from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Detailed information for debugging.
    Debug,
    /// Interesting events, such as a user signing in.
    Info,
    /// Normal but significant events.
    Notice,
    /// Unusual things that are not errors, such as use of a deprecated API.
    Warning,
    /// Runtime errors that need attention but not immediate action.
    Error,
    /// Critical conditions, such as an unavailable component.
    Critical,
    /// Action must be taken immediately, such as the database being down.
    Alert,
    /// The system is unusable.
    Emergency,
}

impl Level {
    /// Parses a level name, ignoring case.
    pub fn parse(name: &str) -> Option<Self> {
        let level = match name.to_ascii_lowercase().as_str() {
            "debug" => Self::Debug,
            "info" => Self::Info,
            "notice" => Self::Notice,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "critical" => Self::Critical,
            "alert" => Self::Alert,
            "emergency" => Self::Emergency,
            _ => return None,
        };
        Some(level)
    }

    /// The upper-case name used in log lines.
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Notice => "NOTICE",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Critical => "CRITICAL",
            Self::Alert => "ALERT",
            Self::Emergency => "EMERGENCY",
        }
    }
}

/// Writes log entries through the app-wide logger. Every method takes
/// anything printable: a `&str`, a `String` or `format_args!(...)`.
pub struct Log;

impl Log {
    /// Logs at [`Level::Debug`].
    pub fn debug(message: impl Display) {
        logger().log(Level::Debug, message);
    }

    /// Logs at [`Level::Info`].
    pub fn info(message: impl Display) {
        logger().log(Level::Info, message);
    }

    /// Logs at [`Level::Notice`].
    pub fn notice(message: impl Display) {
        logger().log(Level::Notice, message);
    }

    /// Logs at [`Level::Warning`].
    pub fn warning(message: impl Display) {
        logger().log(Level::Warning, message);
    }

    /// Logs at [`Level::Error`].
    pub fn error(message: impl Display) {
        logger().log(Level::Error, message);
    }

    /// Logs at [`Level::Critical`].
    pub fn critical(message: impl Display) {
        logger().log(Level::Critical, message);
    }

    /// Logs at [`Level::Alert`].
    pub fn alert(message: impl Display) {
        logger().log(Level::Alert, message);
    }

    /// Logs at [`Level::Emergency`].
    pub fn emergency(message: impl Display) {
        logger().log(Level::Emergency, message);
    }
}

fn logger() -> &'static Logger {
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    LOGGER.get_or_init(|| Logger::from_config(&Config::load()))
}

/// Where entries go and which are kept. [`Log`] uses one built from config;
/// build your own for a separate log file or in tests.
#[derive(Debug)]
pub struct Logger {
    /// `None` is silent.
    min: Option<Level>,
    env: String,
    /// `None` writes to standard error.
    file: Option<Mutex<File>>,
}

impl Logger {
    /// A logger configured by `LOG_LEVEL`, `LOG_CHANNEL` and `APP_ENV`. When
    /// the log file cannot be opened it falls back to standard error and says so.
    pub fn from_config(config: &Config) -> Self {
        let level = config.get("LOG_LEVEL").unwrap_or("debug");
        let min = match level {
            _ if level.eq_ignore_ascii_case("silent") => None,
            _ => Some(Level::parse(level).unwrap_or_else(|| {
                panic!("config key LOG_LEVEL is set but is not a log level: {level}")
            })),
        };
        let env = config.get("APP_ENV").unwrap_or("local");
        match config.get("LOG_CHANNEL").unwrap_or("file") {
            "stderr" => Self::stderr(min, env),
            "file" => Self::file(FILE, min, env).unwrap_or_else(|problem| {
                eprintln!("log: cannot open {FILE} ({problem}); logging to stderr");
                Self::stderr(min, env)
            }),
            other => panic!("config key LOG_CHANNEL is set but is not file or stderr: {other}"),
        }
    }

    /// Appends to `path`, creating it and its folders. `min` of `None` is silent.
    pub fn file(path: impl AsRef<Path>, min: Option<Level>, env: &str) -> std::io::Result<Self> {
        let path = path.as_ref();
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            min,
            env: env.into(),
            file: Some(Mutex::new(file)),
        })
    }

    /// Writes to standard error. `min` of `None` is silent.
    pub fn stderr(min: Option<Level>, env: &str) -> Self {
        Self {
            min,
            env: env.into(),
            file: None,
        }
    }

    /// Writes one entry when `level` is at or above the minimum. Write
    /// failures are ignored: logging never takes the app down.
    pub fn log(&self, level: Level, message: impl Display) {
        if self.min.is_none_or(|min| level < min) {
            return;
        }
        let line = line(now(), &self.env, level, &message.to_string());
        match &self.file {
            Some(file) => {
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = file.write_all(line.as_bytes());
            }
            None => eprint!("{line}"),
        }
    }
}

/// One formatted entry, ending in a newline.
fn line(seconds: u64, env: &str, level: Level, message: &str) -> String {
    let message = message
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    format!(
        "[{}] {env}.{}: {message}\n",
        timestamp(seconds),
        level.name()
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// `YYYY-MM-DD HH:MM:SS` in UTC for Unix `seconds`.
fn timestamp(seconds: u64) -> String {
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // Days to a civil date: Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(timestamp(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(timestamp(1_790_693_195), "2026-09-29 14:46:35");
        assert_eq!(timestamp(4_107_542_399), "2100-02-28 23:59:59");
    }

    #[test]
    fn lines_are_single_and_labelled() {
        assert_eq!(
            line(0, "production", Level::Warning, "disk low\nfake entry\\n"),
            "[1970-01-01 00:00:00] production.WARNING: disk low\\nfake entry\\\\n\n"
        );
    }

    #[test]
    fn file_logger_filters_by_level_and_silent_writes_nothing() {
        let folder = std::env::temp_dir().join(format!("clamp-log-{}", std::process::id()));
        let path = folder.join("nested/app.log");
        let logger = Logger::file(&path, Some(Level::Warning), "test").unwrap();
        logger.log(Level::Info, "skipped");
        logger.log(Level::Warning, "kept");
        logger.log(Level::Emergency, format_args!("kept {}", 2));
        let silent = Logger::file(&path, None, "test").unwrap();
        silent.log(Level::Emergency, "never");
        let written = fs::read_to_string(&path).unwrap();
        fs::remove_dir_all(&folder).unwrap();
        let entries: Vec<_> = written
            .lines()
            .map(|line| line.split("] ").nth(1).unwrap())
            .collect();
        assert_eq!(entries, ["test.WARNING: kept", "test.EMERGENCY: kept 2"]);
    }

    #[test]
    fn config_picks_level_and_channel() {
        let logger = Logger::from_config(&Config::parse(
            "LOG_LEVEL=Error\nLOG_CHANNEL=stderr\nAPP_ENV=prod\n",
        ));
        assert_eq!(
            (logger.min, logger.env.as_str(), logger.file.is_none()),
            (Some(Level::Error), "prod", true)
        );
        let silent = Logger::from_config(&Config::parse("LOG_LEVEL=silent\nLOG_CHANNEL=stderr\n"));
        assert_eq!(silent.min, None);
    }

    #[test]
    #[should_panic(expected = "LOG_LEVEL is set but is not a log level: loud")]
    fn unknown_level_names_the_key() {
        Logger::from_config(&Config::parse("LOG_LEVEL=loud\n"));
    }
}
