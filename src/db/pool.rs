//! The Postgres connection pool behind [`Db`](super::Db) (ADR 0034): idle
//! clients in a mutex-guarded `Vec`, opened on demand up to a cap.

use std::cell::RefCell;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::connection::{Connection, Error, Inner};
use super::postgres::{Client, Config};

/// How long a call waits for a free client before it fails, as long as
/// SQLite's busy timeout.
const PATIENCE: Duration = Duration::from_secs(5);

// ponytail: an idle client the server has closed fails the first call that
// draws it and is dropped then; check or retry on checkout if idle timeouts
// (a proxy, a restart) show up.
pub(super) struct Pool {
    config: Config,
    max: usize,
    patience: Duration,
    state: Mutex<State>,
    freed: Condvar,
}

struct State {
    idle: Vec<Client>,
    /// Clients alive: idle, lent out or being connected.
    open: usize,
}

impl Pool {
    /// A pool of at most `max` clients, with one connected now so a wrong
    /// host or password stops the app at startup.
    pub(super) fn new(config: Config, max: usize) -> Result<Self, Error> {
        let first = Client::connect(&config)?;
        Ok(Self::with_client(config, max, first, PATIENCE))
    }

    fn with_client(config: Config, max: usize, first: Client, patience: Duration) -> Self {
        Self {
            config,
            max: max.max(1),
            patience,
            state: Mutex::new(State {
                idle: vec![first],
                open: 1,
            }),
            freed: Condvar::new(),
        }
    }

    /// Runs `work` on a pooled client. When none can be had, `work` gets a
    /// connection whose every call returns why.
    pub(super) fn with<T>(&self, work: impl FnOnce(&Connection<'_>) -> T) -> T {
        match self.get() {
            Ok(lease) => work(&lease.connection()),
            Err(error) => work(&Connection(Inner::Unavailable(&error))),
        }
    }

    /// A client for as long as the lease lives; a transaction keeps it to its end.
    pub(super) fn get(&self) -> Result<Lease<'_>, Error> {
        let deadline = Instant::now() + self.patience;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(client) = state.idle.pop() {
                return Ok(Lease::new(self, client));
            }
            if state.open < self.max {
                state.open += 1;
                drop(state);
                return match Client::connect(&self.config) {
                    Ok(client) => Ok(Lease::new(self, client)),
                    Err(error) => {
                        self.forget_one();
                        Err(error.into())
                    }
                };
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::new(format!(
                    "no free database connection after {} s (DB_POOL is {})",
                    self.patience.as_secs(),
                    self.max
                )));
            }
            state = self
                .freed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// A client is gone (never connected, or dropped): its slot is free again.
    fn forget_one(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.open -= 1;
        self.freed.notify_one();
    }
}

/// A client taken from the [`Pool`]; goes back when dropped, unless it broke
/// or a panic unwinds through it (its transaction state is then unknown, and
/// closing it makes the server roll back).
pub(super) struct Lease<'a> {
    pool: &'a Pool,
    client: Option<RefCell<Client>>,
}

impl<'a> Lease<'a> {
    fn new(pool: &'a Pool, client: Client) -> Self {
        Self {
            pool,
            client: Some(RefCell::new(client)),
        }
    }

    /// Whether the client is inside a transaction an earlier call began.
    pub(super) fn in_transaction(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|client| client.borrow().in_transaction())
    }

    pub(super) fn connection(&self) -> Connection<'_> {
        let client = self.client.as_ref().expect("a lease holds its client");
        Connection(Inner::Postgres(client))
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let Some(client) = self.client.take().map(RefCell::into_inner) else {
            return;
        };
        if client.is_broken() || std::thread::panicking() {
            drop(client);
            self.pool.forget_one();
        } else {
            let mut state = self
                .pool
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.idle.push(client);
            self.pool.freed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool of `max` clients on the test server, or `None` without one.
    fn pool(max: usize, patience: Duration) -> Option<Pool> {
        let url = std::env::var("RUSTCLAMP_TEST_DATABASE_URL").ok()?;
        let config = Config::parse_url(&url).unwrap();
        let first = Client::connect(&config).unwrap();
        Some(Pool::with_client(config, max, first, patience))
    }

    fn open(pool: &Pool) -> usize {
        pool.state.lock().unwrap().open
    }

    #[test]
    fn a_full_pool_makes_callers_wait_then_fail() {
        let Some(pool) = pool(1, Duration::from_millis(200)) else {
            return;
        };
        let lease = pool.get().unwrap();
        let started = Instant::now();
        let error = pool.get().err().expect("no free client");
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(error.to_string().contains("DB_POOL is 1"), "{error}");
        drop(lease);
        pool.get().unwrap();
        assert_eq!(open(&pool), 1);
    }

    #[test]
    fn a_waiter_takes_the_client_that_comes_back() {
        let Some(pool) = pool(1, Duration::from_secs(5)) else {
            return;
        };
        let lease = pool.get().unwrap();
        std::thread::scope(|threads| {
            let waiter =
                threads.spawn(|| pool.with(|c| c.query_row("SELECT 1", &[], |r| r.get::<i64>(0))));
            std::thread::sleep(Duration::from_millis(100));
            drop(lease);
            assert_eq!(waiter.join().unwrap().unwrap(), 1);
        });
    }

    #[test]
    fn grows_to_the_cap_and_no_further() {
        let Some(pool) = pool(3, Duration::from_millis(100)) else {
            return;
        };
        let leases: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
        assert_eq!(open(&pool), 3);
        assert!(pool.get().is_err());
        drop(leases);
        assert_eq!(pool.state.lock().unwrap().idle.len(), 3);
    }

    #[test]
    fn a_lease_dropped_by_a_panic_or_a_dead_connection_is_not_reused() {
        let Some(pool) = pool(2, Duration::from_secs(1)) else {
            return;
        };
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = pool.get().unwrap();
            panic!("boom");
        }));
        assert!(panicked.is_err());
        assert_eq!(open(&pool), 0, "the connection was closed");
        // The server ends a session; the call that finds out fails, and the
        // pool lets the dead client go instead of lending it again.
        let result = pool.with(|c| {
            let _ = c.execute_batch("SELECT pg_terminate_backend(pg_backend_pid())");
            c.query_row("SELECT 1", &[], |r| r.get::<i64>(0))
        });
        assert!(result.is_err());
        assert_eq!(open(&pool), 0);
        assert_eq!(
            pool.with(|c| c.query_row("SELECT 1", &[], |r| r.get::<i64>(0)))
                .unwrap(),
            1
        );
    }
}
