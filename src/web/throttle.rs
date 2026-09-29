use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{Next, Request, Response, error};

/// Most clients tracked at once. At the cap, expired windows are pruned, then
/// the oldest window is dropped, so memory stays bounded under address rotation.
const CAPACITY: usize = 10_000;

/// A fixed-window rate limiter: at most `max` hits per key in each window.
///
/// Usable on its own (for example from a console command) or as middleware
/// through [`Throttle::middleware`] and [`throttle`].
#[derive(Debug)]
pub struct Throttle {
    max: u32,
    window: Duration,
    trust_forwarded: bool,
    capacity: usize,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Throttle {
    /// At most `max` hits per key in every `window`.
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            trust_forwarded: false,
            capacity: CAPACITY,
            hits: Mutex::default(),
        }
    }

    /// At most `max` hits per key per minute.
    pub fn per_minute(max: u32) -> Self {
        Self::new(max, Duration::from_secs(60))
    }

    /// When `trust` is set, identifies clients by the last `X-Forwarded-For`
    /// address, the one the proxy appended, instead of the connecting peer.
    /// Enable this only behind exactly one proxy that sets the header, such as
    /// nginx; otherwise clients can pick their own key.
    #[must_use]
    pub fn trust_forwarded(mut self, trust: bool) -> Self {
        self.trust_forwarded = trust;
        self
    }

    /// Counts one hit for `key`. Over the limit, returns how long until the
    /// window resets.
    pub fn hit(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // ponytail: O(n) scans, only at capacity; a timing wheel if that ever shows in profiles
        if hits.len() >= self.capacity && !hits.contains_key(key) {
            hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);
            if hits.len() >= self.capacity
                && let Some(oldest) = hits
                    .iter()
                    .min_by_key(|(_, (start, _))| *start)
                    .map(|(k, _)| k.clone())
            {
                hits.remove(&oldest);
            }
        }
        let entry = hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > self.max {
            Err(self.window - now.duration_since(entry.0))
        } else {
            Ok(())
        }
    }

    /// The client key for `request`: its forwarded or peer address.
    fn client(&self, request: &Request) -> String {
        let forwarded = self
            .trust_forwarded
            .then(|| request.header("x-forwarded-for"))
            .flatten()
            .and_then(|list| list.rsplit(',').next())
            .map(str::trim);
        match (forwarded, request.peer) {
            (Some(address), _) => address.to_owned(),
            (None, Some(peer)) => peer.to_string(),
            (None, None) => "unknown".to_owned(),
        }
    }

    /// Middleware answering `429` with `Retry-After` once a client is over the limit.
    pub fn middleware(self) -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
        move |request, next| match self.hit(&self.client(request)) {
            Ok(()) => next(request),
            Err(wait) => {
                let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
                error(429).with_header("Retry-After", &seconds.max(1).to_string())
            }
        }
    }
}

/// Middleware allowing each client `max` requests per `window`, keyed by peer address.
pub fn throttle(
    max: u32,
    window: Duration,
) -> impl Fn(&Request, Next) -> Response + Send + Sync + 'static {
    Throttle::new(max, window).middleware()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::Router;

    #[test]
    fn limits_each_client_per_window() {
        let limiter = Throttle::new(2, Duration::from_secs(60));
        assert!(limiter.hit("a").is_ok());
        assert!(limiter.hit("a").is_ok());
        let wait = limiter.hit("a").unwrap_err();
        assert!(wait > Duration::from_secs(58));
        assert!(limiter.hit("b").is_ok());

        let short = Throttle::new(1, Duration::from_millis(20));
        assert!(short.hit("a").is_ok());
        assert!(short.hit("a").is_err());
        std::thread::sleep(Duration::from_millis(25));
        assert!(short.hit("a").is_ok());
    }

    #[test]
    fn memory_stays_bounded() {
        let mut limiter = Throttle::new(1, Duration::from_millis(50));
        limiter.capacity = 3;
        for key in ["a", "b", "c"] {
            limiter.hit(key).unwrap();
        }
        limiter.hit("d").unwrap();
        let keys = |limiter: &Throttle| limiter.hits.lock().unwrap().len();
        assert_eq!(keys(&limiter), 3, "the oldest window makes room");
        std::thread::sleep(Duration::from_millis(60));
        limiter.hit("e").unwrap();
        assert_eq!(keys(&limiter), 1, "expired windows are pruned first");
    }

    #[test]
    fn middleware_answers_429_with_retry_after() {
        let routes = Router::new()
            .middleware(throttle(1, Duration::from_secs(30)))
            .get("/", |_| Response::text(200, "ok"));
        let peer = "10.0.0.1".parse().unwrap();
        assert_eq!(
            routes.handle(&Request::get("/").with_peer(peer)).status,
            200
        );
        let limited = routes.handle(&Request::get("/").with_peer(peer));
        assert_eq!(limited.status, 429);
        assert_eq!(limited.header("retry-after"), Some("30"));
        let other = "10.0.0.2".parse().unwrap();
        assert_eq!(
            routes.handle(&Request::get("/").with_peer(other)).status,
            200
        );
    }

    #[test]
    fn forwarded_address_is_opt_in() {
        let proxy = "127.0.0.1".parse().unwrap();
        let request = |client: &str| {
            Request::get("/")
                .with_peer(proxy)
                .with_header("X-Forwarded-For", client)
        };
        let direct = Throttle::new(1, Duration::from_secs(60));
        assert_eq!(direct.client(&request("1.1.1.1")), "127.0.0.1");
        let proxied = Throttle::new(1, Duration::from_secs(60)).trust_forwarded(true);
        // nginx appends the real address; anything before it came from the client.
        assert_eq!(proxied.client(&request("6.6.6.6, 1.1.1.1")), "1.1.1.1");
        assert_eq!(proxied.client(&request("1.1.1.1")), "1.1.1.1");
        let routes = crate::web::Router::new()
            .middleware(proxied.middleware())
            .get("/", |_| Response::text(200, "ok"));
        assert_eq!(routes.handle(&request("fake-1, 1.1.1.1")).status, 200);
        assert_eq!(
            routes.handle(&request("fake-2, 1.1.1.1")).status,
            429,
            "spoofed prefixes share one bucket"
        );
    }
}
