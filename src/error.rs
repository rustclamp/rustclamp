//! Framework errors as exit codes and problem JSON.
//!
//! An app's `main` maps errors to a process exit code and an error message;
//! [`ExitError`] gives the facade's own errors ([`ConfigError`], [`DbError`],
//! [`ServeError`]) one answer for both, and [`run`] prints it and exits:
//!
//! ```no_run
//! use rustclamp::error::{ExitError, Problem};
//!
//! #[derive(Debug)]
//! struct NoInput;
//!
//! impl std::fmt::Display for NoInput {
//!     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//!         f.write_str("no input file")
//!     }
//! }
//!
//! impl std::error::Error for NoInput {}
//!
//! impl ExitError for NoInput {
//!     fn exit_code(&self) -> i32 {
//!         66
//!     }
//!
//!     fn problem(&self) -> Problem {
//!         Problem::new(404, "No input", self.to_string())
//!     }
//! }
//!
//! fn main() {
//!     rustclamp::error::run(|| Err::<(), _>(NoInput));
//! }
//! ```
//!
//! Exit codes follow `sysexits.h`: `78` for configuration, `69` for a service
//! that is not available and `74` for input/output.
//!
//! [`ConfigError`]: crate::config::ConfigError
//! [`DbError`]: crate::db::DbError
//! [`ServeError`]: crate::web::ServeError

/// An RFC 9457 problem: the `type`, `title`, `status` and `detail` shape of
/// `rustclamp-http`'s `HttpError` and [`web::problem`](crate::web::problem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// An HTTP status code that classes the failure, such as `500`.
    pub status: u16,
    /// A short, stable summary.
    pub title: &'static str,
    /// What went wrong, for the person reading it.
    pub detail: String,
}

impl Problem {
    /// The problem `title` and `detail` at `status`.
    pub fn new(status: u16, title: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            title,
            detail: detail.into(),
        }
    }

    /// The `application/problem+json` body.
    pub fn json(&self) -> String {
        format!(
            r#"{{"type":"about:blank","title":"{}","status":{},"detail":"{}"}}"#,
            json_escape(self.title),
            self.status,
            json_escape(&self.detail)
        )
    }
}

pub(crate) fn json_escape(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// An error that knows its process exit code and its [`Problem`], so a
/// program does not keep its own table of both.
pub trait ExitError: std::error::Error {
    /// The process exit code for this error; never `0`.
    fn exit_code(&self) -> i32;

    /// This error as a [`Problem`].
    fn problem(&self) -> Problem;
}

/// Runs `main`; on an error prints `error: {message}` to stderr and exits with
/// its [`exit_code`](ExitError::exit_code).
pub fn run<E: ExitError>(main: impl FnOnce() -> Result<(), E>) {
    if let Err(error) = main() {
        eprintln!("error: {}", error.problem().detail);
        std::process::exit(error.exit_code());
    }
}

/// [`run`], printing the [`Problem`] as one line of JSON, for programs whose
/// output is read by other programs.
pub fn run_json<E: ExitError>(main: impl FnOnce() -> Result<(), E>) {
    if let Err(error) = main() {
        eprintln!("{}", error.problem().json());
        std::process::exit(error.exit_code());
    }
}

#[cfg(feature = "config")]
impl ExitError for crate::config::ConfigError {
    fn exit_code(&self) -> i32 {
        78
    }

    fn problem(&self) -> Problem {
        Problem::new(500, "Configuration error", self.to_string())
    }
}

#[cfg(feature = "db")]
impl ExitError for crate::db::DbError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Engine(_) => 78,
            Self::Open { .. } => 74,
        }
    }

    fn problem(&self) -> Problem {
        match self {
            Self::Engine(_) => Problem::new(500, "Configuration error", self.to_string()),
            Self::Open { .. } => Problem::new(503, "Database unavailable", self.to_string()),
        }
    }
}

#[cfg(feature = "web")]
impl ExitError for crate::web::ServeError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Config(error) => error.exit_code(),
            Self::Bind { .. } => 69,
        }
    }

    fn problem(&self) -> Problem {
        match self {
            Self::Config(error) => error.problem(),
            Self::Bind { .. } => Problem::new(503, "Port unavailable", self.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_json_has_the_http_error_shape_and_escapes() {
        assert_eq!(
            Problem::new(503, "Down", "say \"hi\"\n").json(),
            r#"{"type":"about:blank","title":"Down","status":503,"detail":"say \"hi\"\u000a"}"#
        );
    }

    #[cfg(feature = "web")]
    #[test]
    fn framework_errors_answer_with_codes_and_problems() {
        use crate::config::ConfigError;
        use crate::web::ServeError;

        let missing = ConfigError::Missing("PORT".into());
        assert_eq!((missing.exit_code(), missing.problem().status), (78, 500));
        let serve = ServeError::Config(missing.clone());
        assert_eq!(serve.exit_code(), 78);
        assert_eq!(serve.problem(), missing.problem());
        let busy = ServeError::Bind {
            port: 80,
            source: std::io::ErrorKind::AddrInUse.into(),
        };
        assert_eq!((busy.exit_code(), busy.problem().status), (69, 503));
        assert!(busy.problem().detail.contains("Port 80"));
    }

    #[cfg(feature = "db")]
    #[test]
    fn database_errors_split_config_from_io() {
        use crate::db::DbError;
        let engine = DbError::Engine("mysql".into());
        assert_eq!(engine.exit_code(), 78);
        assert!(engine.problem().detail.contains("mysql"));
    }
}
