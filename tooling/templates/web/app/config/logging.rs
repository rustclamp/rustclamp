//! Logging, read once at startup from `.env` and the environment.
//! `.env.example` lists every key.

use rustclamp::config::Config;
use rustclamp::log::Settings;

/// How the app logs. Change it in `.env`, not here.
pub fn logging(config: &Config) -> Settings {
    Settings {
        // `LOG_LEVEL`: the lowest level written, or `silent`.
        level: config.get("LOG_LEVEL").unwrap_or("debug").into(),
        // `LOG_CHANNEL`: `single` (storage/logs/app.log), `daily`, `stderr`
        // or `stack`.
        channel: config.get("LOG_CHANNEL").unwrap_or("single").into(),
        // `LOG_STACK`: the channels `stack` writes to.
        stack: config.get("LOG_STACK").unwrap_or("single").into(),
        // `LOG_DAILY_DAYS`: how many daily files to keep.
        daily_days: config.get_or("LOG_DAILY_DAYS", 14),
        // `APP_ENV`: the environment name in each line.
        env: config.get("APP_ENV").unwrap_or("local").into(),
    }
}
