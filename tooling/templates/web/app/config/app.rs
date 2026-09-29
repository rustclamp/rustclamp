//! Typed settings, read once at startup from `.env` and the environment.
//! `.env.example` lists every key.

use rustclamp::config::Config;
use rustclamp::web::Throttle;

/// The app's settings. Change them in `.env`, not here.
pub struct Settings {
    /// `API_PER_MINUTE`: requests per client per minute to `/api/*`.
    pub api_per_minute: u32,
    /// `TRUST_PROXY`: key rate limits by `X-Forwarded-For`. Only behind nginx.
    pub trust_proxy: bool,
}

impl Settings {
    pub fn from(config: &Config) -> Self {
        Self {
            api_per_minute: config.get_or("API_PER_MINUTE", 60),
            trust_proxy: config.get_or("TRUST_PROXY", false),
        }
    }

    /// A limiter allowing `max` requests per client per minute.
    pub fn per_minute(&self, max: u32) -> Throttle {
        Throttle::per_minute(max).trust_forwarded(self.trust_proxy)
    }
}
