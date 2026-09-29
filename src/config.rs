//! App configuration from a `.env` file and the process environment.
//!
//! Enabled by the `config` feature (the `web` feature includes it). Real
//! environment variables win over `.env`, so production sets values in the
//! environment and development keeps them in `.env`. Loading never modifies
//! the process environment.
//!
//! ```
//! use rustclamp::config::Config;
//!
//! let config = Config::parse("APP_NAME=Site\nMAIL_PER_DAY=100\n");
//! assert_eq!(config.get("APP_NAME"), Some("Site"));
//! assert_eq!(config.get_or("MAIL_PER_DAY", 50), 100);
//! assert_eq!(config.get_or("MISSING", 50), 50);
//! ```

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Configuration values by key. `Debug` shows keys only, never values.
#[derive(Clone, Default)]
pub struct Config {
    values: HashMap<String, String>,
}

impl Config {
    /// Reads `.env` from the working directory, if present, then the process
    /// environment on top of it.
    pub fn load() -> Self {
        let mut config = std::fs::read_to_string(".env")
            .map(|text| Self::parse(&text))
            .unwrap_or_default();
        config.values.extend(std::env::vars());
        config
    }

    /// Parses `.env` text: `KEY=value` lines, optional `export ` prefixes,
    /// `#` comments and blank lines. Matching quotes around a value are removed.
    pub fn parse(text: &str) -> Self {
        // ponytail: no `${VAR}` interpolation or multi-line values; add when an app needs them
        let values = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let line = line.strip_prefix("export ").unwrap_or(line);
                let (key, value) = line.split_once('=')?;
                let value = value.trim();
                let unquoted = [('"', '"'), ('\'', '\'')]
                    .iter()
                    .find_map(|(open, close)| value.strip_prefix(*open)?.strip_suffix(*close));
                Some((key.trim().to_owned(), unquoted.unwrap_or(value).to_owned()))
            })
            .collect();
        Self { values }
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// The value of `key` parsed as `T`, or `default` when it is not set.
    ///
    /// # Panics
    ///
    /// When `key` is set but does not parse as `T`: a wrong value should stop
    /// the app at startup, naming the key, rather than be silently replaced.
    pub fn get_or<T: FromStr>(&self, key: &str, default: T) -> T {
        match self.get(key) {
            None => default,
            Some(value) => value.parse().unwrap_or_else(|_| {
                panic!(
                    "config key {key} is set but is not a valid {}",
                    std::any::type_name::<T>()
                )
            }),
        }
    }

    /// Whether `APP_ENV` is `production`: settings that protect users
    /// default to on there.
    pub fn is_production(&self) -> bool {
        self.get("APP_ENV") == Some("production")
    }

    /// The value of `key`.
    ///
    /// # Panics
    ///
    /// When `key` is not set, naming the key.
    pub fn require(&self, key: &str) -> &str {
        self.get(key)
            .unwrap_or_else(|| panic!("config key {key} is required but not set"))
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut keys: Vec<_> = self.values.keys().collect();
        keys.sort();
        f.debug_struct("Config")
            .field("keys", &keys)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_syntax() {
        let config = Config::parse(
            "# comment\n\nexport APP=site\nNAME=\"Neo C\"\nSINGLE='a=b'\n  SPACED = x \nbroken line\n",
        );
        assert_eq!(config.get("APP"), Some("site"));
        assert_eq!(config.get("NAME"), Some("Neo C"));
        assert_eq!(config.get("SINGLE"), Some("a=b"));
        assert_eq!(config.get("SPACED"), Some("x"));
        assert_eq!(config.get("broken line"), None);
    }

    #[test]
    fn debug_hides_values() {
        let shown = format!("{:?}", Config::parse("APP_KEY=secret\n"));
        assert!(shown.contains("APP_KEY"));
        assert!(!shown.contains("secret"));
    }

    #[test]
    #[should_panic(expected = "config key PORT is set but is not a valid u16")]
    fn bad_values_name_the_key() {
        Config::parse("PORT=eighty\n").get_or::<u16>("PORT", 8080);
    }

    #[test]
    #[should_panic(expected = "config key APP_KEY is required")]
    fn require_names_the_missing_key() {
        Config::default().require("APP_KEY");
    }
}
