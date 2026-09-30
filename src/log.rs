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
//! An app sets the logger up at startup with [`Log::init`], from its
//! `app/config/logging.rs`. Otherwise settings come from `.env` and the
//! environment ([`Config::load`]), read on the first log call:
//!
//! - `LOG_LEVEL`: the lowest level written, `debug` by default. `silent`
//!   turns logging off.
//! - `LOG_CHANNEL`, as in Laravel:
//!   - `single` (`file` is the old name) appends to `storage/logs/app.log`.
//!     It is the default when the working directory has a `storage/` folder,
//!     as an app's does; elsewhere, such as a CLI run from any folder, the
//!     default is `stderr`, so no stray `storage/logs` is created;
//!   - `daily` writes `storage/logs/app-YYYY-MM-DD.log`, one file per UTC
//!     day, keeping the newest `LOG_DAILY_DAYS` (14 by default);
//!   - `stderr` writes to standard error, for systemd or containers;
//!   - `stack` writes to every channel in `LOG_STACK`, such as `daily,stderr`.
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
use crate::utc::timestamp;

/// The log file used by the `single` channel.
pub const FILE: &str = "storage/logs/app.log";
/// The folder the `daily` channel writes `app-YYYY-MM-DD.log` files to.
pub const DAILY: &str = "storage/logs";

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
    /// Makes `logger` the app-wide logger. Call it first thing in `main`.
    ///
    /// # Panics
    ///
    /// When a logger is already in place, because something logged earlier:
    /// those entries went elsewhere, which should not pass unnoticed.
    pub fn init(logger: Logger) {
        assert!(
            LOGGER.set(logger).is_ok(),
            "Log::init called after the logger was set up; call it first in main"
        );
    }

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

/// Middleware logging every request and its status at `debug` level:
/// `router.middleware(rustclamp::log::request_log)`.
#[cfg(feature = "web")]
pub fn request_log(request: &crate::web::Request, next: crate::web::Next) -> crate::web::Response {
    let response = next(request);
    Log::debug(format_args!(
        "{} {} {}",
        request.method, request.path, response.status
    ));
    response
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

fn logger() -> &'static Logger {
    LOGGER.get_or_init(|| Logger::from_config(&Config::load()))
}

/// Where entries go and which are kept. [`Log`] uses one built from config;
/// build your own for a separate log file or in tests.
#[derive(Debug)]
pub struct Logger {
    /// `None` is silent.
    min: Option<Level>,
    env: String,
    sinks: Vec<Sink>,
}

#[derive(Debug)]
enum Sink {
    Stderr,
    Single(Mutex<File>),
    Daily {
        folder: std::path::PathBuf,
        days: usize,
        /// The day's date and its open file.
        today: Mutex<Option<(String, File)>>,
    },
}

/// How the app logs. An app builds it in `app/config/logging.rs`.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The lowest level written, such as `debug`, or `silent`.
    pub level: String,
    /// `single`, `daily`, `stderr` or `stack`.
    pub channel: String,
    /// The channels `stack` writes to, such as `daily,stderr`.
    pub stack: String,
    /// How many files the `daily` channel keeps.
    pub daily_days: usize,
    /// The environment name in each line, such as `local`.
    pub env: String,
}

impl Settings {
    /// `LOG_LEVEL` (`debug`), `LOG_CHANNEL` (`single` when `./storage`
    /// exists, else `stderr`), `LOG_STACK` (`single`), `LOG_DAILY_DAYS` (14)
    /// and `APP_ENV` (`local`).
    pub fn from_config(config: &Config) -> Self {
        // ponytail: the same defaults as the web template's
        // `app/config/logging.rs`, for apps without one
        Self {
            level: config.get("LOG_LEVEL").unwrap_or("debug").into(),
            channel: config
                .get("LOG_CHANNEL")
                .unwrap_or(default_channel(Path::new("storage")))
                .into(),
            stack: config.get("LOG_STACK").unwrap_or("single").into(),
            daily_days: config.get_or("LOG_DAILY_DAYS", 14),
            env: config.get("APP_ENV").unwrap_or("local").into(),
        }
    }
}

/// `single` when the app's `storage` folder exists, else `stderr` (#47).
fn default_channel(storage: &Path) -> &'static str {
    if storage.is_dir() { "single" } else { "stderr" }
}

impl Logger {
    /// A logger configured by `LOG_LEVEL`, `LOG_CHANNEL`, `LOG_STACK`,
    /// `LOG_DAILY_DAYS` and `APP_ENV`; see [`Logger::new`].
    pub fn from_config(config: &Config) -> Self {
        Self::new(&Settings::from_config(config))
    }

    /// A logger configured by `settings`. When a log file cannot be opened
    /// it falls back to standard error and says so.
    ///
    /// # Panics
    ///
    /// On an unknown level or channel, naming the key.
    pub fn new(settings: &Settings) -> Self {
        let level = settings.level.as_str();
        let min = match level {
            _ if level.eq_ignore_ascii_case("silent") => None,
            _ => Some(Level::parse(level).unwrap_or_else(|| {
                panic!("config key LOG_LEVEL is set but is not a log level: {level}")
            })),
        };
        let days = settings.daily_days;
        let sink = |channel: &str, key: &str| match channel.trim() {
            "stderr" => Sink::Stderr,
            "single" | "file" => single(Path::new(FILE)).unwrap_or_else(|problem| {
                eprintln!("log: cannot open {FILE} ({problem}); logging to stderr");
                Sink::Stderr
            }),
            "daily" => daily(Path::new(DAILY), days),
            other => {
                panic!("config key {key} is set but is not single, daily, stderr or stack: {other}")
            }
        };
        let sinks = match settings.channel.as_str() {
            "stack" => settings
                .stack
                .split(',')
                .map(|channel| sink(channel, "LOG_STACK"))
                .collect(),
            channel => vec![sink(channel, "LOG_CHANNEL")],
        };
        Self {
            min,
            env: settings.env.clone(),
            sinks,
        }
    }

    /// Appends to `path`, creating it and its folders. `min` of `None` is silent.
    pub fn file(path: impl AsRef<Path>, min: Option<Level>, env: &str) -> std::io::Result<Self> {
        Ok(Self {
            min,
            env: env.into(),
            sinks: vec![single(path.as_ref())?],
        })
    }

    /// Writes `app-YYYY-MM-DD.log` files in `folder`, one per UTC day,
    /// keeping the newest `days`. `min` of `None` is silent.
    pub fn daily(folder: impl AsRef<Path>, days: usize, min: Option<Level>, env: &str) -> Self {
        Self {
            min,
            env: env.into(),
            sinks: vec![daily(folder.as_ref(), days)],
        }
    }

    /// Writes to standard error. `min` of `None` is silent.
    pub fn stderr(min: Option<Level>, env: &str) -> Self {
        Self {
            min,
            env: env.into(),
            sinks: vec![Sink::Stderr],
        }
    }

    /// Writes one entry when `level` is at or above the minimum. Write
    /// failures are ignored: logging never takes the app down.
    pub fn log(&self, level: Level, message: impl Display) {
        if self.min.is_none_or(|min| level < min) {
            return;
        }
        let seconds = now();
        let message = message.to_string();
        #[cfg(feature = "uuid")]
        let message = match reference() {
            Some(reference) => format!("{message} ref={reference}"),
            None => message,
        };
        let line = line(seconds, &self.env, level, &message);
        for sink in &self.sinks {
            sink.write(seconds, &line);
        }
    }
}

fn single(path: &Path) -> std::io::Result<Sink> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Sink::Single(Mutex::new(file)))
}

fn daily(folder: &Path, days: usize) -> Sink {
    Sink::Daily {
        folder: folder.to_path_buf(),
        days: days.max(1),
        today: Mutex::new(None),
    }
}

impl Sink {
    fn write(&self, seconds: u64, line: &str) {
        match self {
            Self::Stderr => eprint!("{line}"),
            Self::Single(file) => {
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = file.write_all(line.as_bytes());
            }
            Self::Daily {
                folder,
                days,
                today,
            } => {
                let date = timestamp(seconds)[..10].to_owned();
                let mut today = today
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if today.as_ref().is_none_or(|(open, _)| *open != date) {
                    let path = folder.join(format!("app-{date}.log"));
                    let opened = fs::create_dir_all(folder)
                        .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
                    match opened {
                        Ok(file) => *today = Some((date, file)),
                        Err(problem) => {
                            eprint!("log: cannot open {} ({problem}): {line}", path.display());
                            return;
                        }
                    }
                    prune(folder, *days);
                }
                if let Some((_, file)) = today.as_mut() {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }
}

/// Deletes all but the newest `days` daily files in `folder`.
fn prune(folder: &Path, days: usize) {
    let mut files: Vec<_> = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == "app-YYYY-MM-DD.log".len()
                        && name.starts_with("app-")
                        && name.ends_with(".log")
                })
        })
        .collect();
    // The date in the name sorts oldest first.
    files.sort();
    let excess = files.len().saturating_sub(days);
    for old in &files[..excess] {
        let _ = fs::remove_file(old);
    }
}

#[cfg(feature = "uuid")]
thread_local! {
    static REFERENCE: std::cell::Cell<Option<rustclamp_core::Reference>> =
        const { std::cell::Cell::new(None) };
}

/// Runs `work` with `reference` added to every entry this thread logs
/// meanwhile, as `ref=<reference>` (ADR 0021). The web server runs each
/// request in one.
// ponytail: per thread, so threads a handler spawns log without it; pass the
// reference along when that matters
#[cfg(feature = "web")]
pub(crate) fn with_reference<T>(
    reference: rustclamp_core::Reference,
    work: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<rustclamp_core::Reference>);
    impl Drop for Restore {
        fn drop(&mut self) {
            REFERENCE.set(self.0);
        }
    }
    let _restore = Restore(REFERENCE.replace(Some(reference)));
    work()
}

/// The reference [`with_reference`] set for this thread, if any.
#[cfg(feature = "uuid")]
pub(crate) fn reference() -> Option<rustclamp_core::Reference> {
    REFERENCE.get()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_is_a_file_only_inside_an_app() {
        let folder = std::env::temp_dir().join(format!("rustclamp-log-{}", std::process::id()));
        assert_eq!(default_channel(&folder), "stderr");
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(default_channel(&folder), "single");
        let _ = fs::remove_dir_all(folder);
    }

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

    #[cfg(feature = "web")]
    #[test]
    fn entries_in_a_reference_scope_carry_it() {
        let folder = std::env::temp_dir().join(format!("clamp-ref-{}", std::process::id()));
        let path = folder.join("app.log");
        let logger = Logger::file(&path, Some(Level::Debug), "test").unwrap();
        let reference = rustclamp_core::Reference::new();
        with_reference(reference, || logger.log(Level::Info, "inside"));
        logger.log(Level::Info, "outside");
        let written = fs::read_to_string(&path).unwrap();
        fs::remove_dir_all(&folder).unwrap();
        let entries: Vec<_> = written
            .lines()
            .map(|line| line.split("] ").nth(1).unwrap())
            .collect();
        assert_eq!(
            entries,
            [
                format!("test.INFO: inside ref={reference}"),
                "test.INFO: outside".to_owned()
            ]
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
            (logger.min, logger.env.as_str()),
            (Some(Level::Error), "prod")
        );
        assert!(matches!(logger.sinks.as_slice(), [Sink::Stderr]));
        let silent = Logger::from_config(&Config::parse("LOG_LEVEL=silent\nLOG_CHANNEL=stderr\n"));
        assert_eq!(silent.min, None);
    }

    #[test]
    fn stack_writes_to_each_channel() {
        let logger = Logger::from_config(&Config::parse(
            "LOG_CHANNEL=stack\nLOG_STACK=daily, stderr\n",
        ));
        assert!(matches!(
            logger.sinks.as_slice(),
            [Sink::Daily { days: 14, .. }, Sink::Stderr]
        ));
    }

    #[test]
    #[should_panic(expected = "LOG_STACK is set but is not single, daily, stderr or stack: loud")]
    fn unknown_stack_channel_names_the_key() {
        Logger::from_config(&Config::parse("LOG_CHANNEL=stack\nLOG_STACK=stderr,loud\n"));
    }

    #[test]
    fn daily_files_rotate_by_date_and_keep_the_newest() {
        let folder = std::env::temp_dir().join(format!("clamp-daily-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        for old in ["app-2026-01-01.log", "app-2026-01-02.log", "notes.log"] {
            fs::write(folder.join(old), "").unwrap();
        }
        let sink = daily(&folder, 2);
        sink.write(1_790_693_195, "one\n"); // 2026-09-29
        sink.write(1_790_693_196, "two\n");
        let mut names: Vec<String> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let today = fs::read_to_string(folder.join("app-2026-09-29.log")).unwrap();
        fs::remove_dir_all(&folder).unwrap();
        assert_eq!(
            names,
            ["app-2026-01-02.log", "app-2026-09-29.log", "notes.log"]
        );
        assert_eq!(today, "one\ntwo\n");
    }

    #[test]
    #[should_panic(expected = "LOG_LEVEL is set but is not a log level: loud")]
    fn unknown_level_names_the_key() {
        Logger::from_config(&Config::parse("LOG_LEVEL=loud\n"));
    }
}
