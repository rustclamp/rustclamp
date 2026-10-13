//! The Postgres connection pool behind [`Db`](super::Db) (ADR 0034): idle
//! clients in a mutex-guarded `Vec`, opened on demand up to a cap.

use std::cell::RefCell;
use std::rc::Rc;
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
        let mut work = Some(work);
        let run =
            |connection: &Connection<'_>, _nested| (work.take().expect("runs once"))(connection);
        match self.pinned(run) {
            Ok(value) => value,
            Err(error) => (work.take().expect("not run"))(&Connection(Inner::Unavailable(&error))),
        }
    }

    /// Runs `work` on one client, which every [`with`](Self::with) and
    /// `pinned` on this thread meanwhile shares, so a transaction sees its
    /// own nested `Db` calls. `work` is told whether it is that nested call.
    /// Other threads, and `Db::blocking`'s, get clients of their own.
    pub(super) fn pinned<T>(
        &self,
        work: impl FnOnce(&Connection<'_>, bool) -> T,
    ) -> Result<T, Error> {
        let here = PINNED.with(|pinned| {
            pinned
                .borrow()
                .iter()
                .find(|(pool, _)| std::ptr::eq(*pool, self))
                .map(|(_, client)| client.clone())
        });
        if let Some(client) = here {
            return Ok(work(&Connection(Inner::Postgres(&client)), true));
        }
        let lease = self.get()?;
        PINNED.with(|pinned| pinned.borrow_mut().push((self, lease.client())));
        let _unpin = Unpin(self);
        Ok(work(&lease.connection(), false))
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

/// A client shared by one thread's nested `Db` calls.
type Shared = Rc<RefCell<Client>>;

thread_local! {
    /// The client each pool lent this thread's current `Db` call or transaction.
    static PINNED: RefCell<Vec<(*const Pool, Shared)>> = const { RefCell::new(Vec::new()) };
}

/// Ends [`Pool::pinned`]'s sharing, also when `work` panics.
struct Unpin<'a>(&'a Pool);

impl Drop for Unpin<'_> {
    fn drop(&mut self) {
        PINNED.with(|pinned| {
            pinned
                .borrow_mut()
                .retain(|(pool, _)| !std::ptr::eq(*pool, self.0));
        });
    }
}

/// A client taken from the [`Pool`]; goes back when dropped, unless it broke,
/// a panic unwinds through it, or a `ROLLBACK` of a transaction left open
/// fails (its state is then unknown, and closing it makes the server roll back).
pub(super) struct Lease<'a> {
    pool: &'a Pool,
    client: Option<Rc<RefCell<Client>>>,
}

impl<'a> Lease<'a> {
    fn new(pool: &'a Pool, client: Client) -> Self {
        Self {
            pool,
            client: Some(Rc::new(RefCell::new(client))),
        }
    }

    fn client(&self) -> Shared {
        self.client.clone().expect("a lease holds its client")
    }

    pub(super) fn connection(&self) -> Connection<'_> {
        let client = self.client.as_ref().expect("a lease holds its client");
        Connection(Inner::Postgres(client))
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        // Only this lease holds the client by now: `Unpin` ran before it.
        let client = self
            .client
            .take()
            .and_then(|client| Rc::try_unwrap(client).ok());
        let Some(mut client) = client.map(RefCell::into_inner) else {
            return self.pool.forget_one();
        };
        // A transaction `work` left open (a raw `BEGIN`) must not reach the next caller.
        let clean = !client.is_broken()
            && !std::thread::panicking()
            && (!client.in_transaction() || client.execute_batch("ROLLBACK").is_ok());
        if clean && !client.is_broken() {
            let mut state = self
                .pool
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.idle.push(client);
            self.pool.freed.notify_one();
        } else {
            drop(client);
            self.pool.forget_one();
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

    #[test]
    fn a_transaction_left_open_is_rolled_back_before_reuse() {
        let Some(pool) = pool(1, Duration::from_secs(1)) else {
            return;
        };
        let assigned = |pool: &Pool| {
            pool.with(|c| {
                c.query_row(
                    "SELECT pg_current_xact_id_if_assigned() IS NOT NULL",
                    &[],
                    |r| r.get::<bool>(0),
                )
            })
            .unwrap()
        };
        pool.with(|c| c.execute_batch("BEGIN; SELECT pg_current_xact_id()"))
            .unwrap();
        assert!(
            !assigned(&pool),
            "the next caller must not inherit the transaction"
        );
        assert_eq!(open(&pool), 1);
    }

    #[test]
    fn nested_calls_share_the_client_even_in_a_pool_of_one() {
        let Some(pool) = pool(1, Duration::from_millis(200)) else {
            return;
        };
        let result = pool.pinned(|outer, nested| {
            assert!(!nested);
            outer.execute_batch("BEGIN").unwrap();
            let inside = pool.with(|c| c.query_row("SELECT 1", &[], |r| r.get::<i64>(0)));
            pool.pinned(|_, nested| assert!(nested)).unwrap();
            // A thread of its own does not get the pinned client.
            let other =
                std::thread::scope(|threads| threads.spawn(|| pool.get().is_err()).join().unwrap());
            outer.execute_batch("ROLLBACK").unwrap();
            (inside.unwrap(), other)
        });
        assert_eq!(result.unwrap(), (1, true));
        pool.get().unwrap();
    }
}
