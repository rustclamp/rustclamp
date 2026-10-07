//! Background jobs (ADR 0019), with the `queue` feature.
//!
//! The app names its handlers ([`App::jobs`](crate::web::App::jobs)); a
//! request dispatches by name, `request.queue().dispatch("thumbnail", "{\"id\":7}")`,
//! and a worker thread runs the job after the response. Delivery is at least
//! once: a job whose worker died runs again after `QUEUE_RETRY_AFTER`, so
//! handlers must be idempotent. A failed attempt (an `Err` or a panic) is
//! retried after 10 s, 60 s, then 300 s, up to `QUEUE_TRIES` attempts; the
//! last failure moves the job to the `failed_jobs` table and logs an error.
//!
//! Settings:
//!
//! - `QUEUE_CONNECTION`: `database` (the `jobs` table, default) or `redis`
//!   (with the `redis` feature, at `REDIS_URL`, keys starting with
//!   `REDIS_PREFIX`, which `APP_ENV=production` requires).
//! - `QUEUE_RETRY_AFTER`: seconds before a reserved job counts as abandoned
//!   (90). A std thread cannot be stopped, so set it above the slowest job,
//!   or that job runs alongside its own retry.
//! - `QUEUE_TRIES`: attempts in total (3).
//! - `QUEUE_WORKERS`: worker threads beside the HTTP server: 1, or 0 when
//!   `APP_ENV=production`, where the binary started with `queue:work` runs
//!   them as their own service.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::db::Db;
use crate::db::sqlite::{OptionalExtension, params};
use crate::log::Log;
use crate::web::Shutdown;

/// Runs one job: `Ok` when done, `Err` (or a panic) to retry it.
pub type Handler = fn(&Job<'_>) -> Result<(), String>;

/// A job being run, as its handler sees it.
#[derive(Debug)]
pub struct Job<'a> {
    /// The name it was dispatched under.
    pub name: &'a str,
    /// What was dispatched, in the app's format (usually JSON).
    pub payload: &'a str,
    /// 1 on the first run, 2 on the first retry, and so on.
    pub attempt: u32,
    /// The app's database.
    pub db: &'a Db,
}

/// Seconds before the first, second and later retries.
const BACKOFF: [i64; 3] = [10, 60, 300];

/// The app's queue: shared with handlers as router state
/// ([`App`](crate::web::App) adds it), read with `request.queue()`.
/// Clones share the handlers and the store.
#[derive(Clone, Debug)]
pub struct Queue {
    db: Db,
    handlers: Arc<HashMap<&'static str, Handler>>,
    tries: u32,
    retry_after: i64,
    driver: Driver,
}

#[derive(Clone, Debug)]
enum Driver {
    Database,
    #[cfg(feature = "redis")]
    Redis {
        redis: crate::redis::Redis,
        url: String,
        prefix: String,
    },
}

/// Why a queue call failed.
#[derive(Debug)]
pub enum Error {
    /// No handler has this name: nothing would ever run the job.
    Unknown(String),
    /// The settings cannot work, such as `QUEUE_CONNECTION=redis` without
    /// `REDIS_PREFIX` in production.
    Config(String),
    /// The database failed.
    Db(crate::db::sqlite::Error),
    /// Redis failed or could not be reached.
    #[cfg(feature = "redis")]
    Redis(crate::redis::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "queue: no job handler named {name:?}"),
            Self::Config(why) => write!(f, "queue: {why}"),
            Self::Db(error) => write!(f, "queue: {error}"),
            #[cfg(feature = "redis")]
            Self::Redis(error) => write!(f, "queue: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(error) => Some(error),
            #[cfg(feature = "redis")]
            Self::Redis(error) => Some(error),
            _ => None,
        }
    }
}

impl From<crate::db::sqlite::Error> for Error {
    fn from(error: crate::db::sqlite::Error) -> Self {
        Self::Db(error)
    }
}

#[cfg(feature = "redis")]
impl From<crate::redis::Error> for Error {
    fn from(error: crate::redis::Error) -> Self {
        Self::Redis(error)
    }
}

/// A reserved job. `token` finds it again: the row id, or the Redis member.
struct Reserved {
    token: String,
    name: String,
    payload: String,
    attempt: u32,
}

/// Moves due delayed jobs and expired reservations back to the list, then
/// pops the next job, counts the attempt (a member is `"<attempts> <id>
/// <name>\n<payload>"`) and reserves it, all atomically.
#[cfg(feature = "redis")]
const RESERVE: &str = "\
local function requeue(from, upto)
  for _, job in ipairs(redis.call('ZRANGEBYSCORE', from, '-inf', upto)) do
    redis.call('ZREM', from, job)
    redis.call('RPUSH', KEYS[1], job)
  end
end
requeue(KEYS[2], ARGV[1])
requeue(KEYS[3], ARGV[1] - ARGV[2])
local job = redis.call('LPOP', KEYS[1])
if not job then return false end
local space = string.find(job, ' ')
job = (tonumber(string.sub(job, 1, space - 1)) + 1) .. string.sub(job, space)
redis.call('ZADD', KEYS[3], ARGV[1], job)
return job";

impl Queue {
    /// The queue `QUEUE_*` in `config` describes, running `jobs` by name and
    /// recording failures in `db`. Creates the `jobs` and `failed_jobs`
    /// tables when they are missing. Nothing connects to Redis until the
    /// first call.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unknown `QUEUE_CONNECTION`, or Redis without
    /// `REDIS_PREFIX` when `APP_ENV=production` (apps sharing a Redis would
    /// run each other's jobs); [`Error::Db`] when the tables cannot be made.
    ///
    /// # Panics
    ///
    /// When `QUEUE_TRIES` or `QUEUE_RETRY_AFTER` is set but not a number.
    pub fn new(config: &Config, db: Db, jobs: Vec<(&'static str, Handler)>) -> Result<Self, Error> {
        let driver = match config.get("QUEUE_CONNECTION").unwrap_or("database") {
            "database" => Driver::Database,
            #[cfg(feature = "redis")]
            "redis" => {
                let prefix = config.get("REDIS_PREFIX").unwrap_or("").to_owned();
                if prefix.is_empty() && config.is_production() {
                    return Err(Error::Config(
                        "REDIS_PREFIX is required with APP_ENV=production: apps sharing a Redis would run each other's jobs".into(),
                    ));
                }
                let url = config
                    .get("REDIS_URL")
                    .unwrap_or("redis://127.0.0.1:6379")
                    .to_owned();
                Driver::Redis {
                    redis: crate::redis::Redis::connect(&url)?,
                    url,
                    prefix,
                }
            }
            other => {
                return Err(Error::Config(format!(
                    "QUEUE_CONNECTION={other:?}: expected database, or redis with the redis feature"
                )));
            }
        };
        db.with(create_tables)?;
        Ok(Self {
            db,
            handlers: Arc::new(jobs.into_iter().collect()),
            tries: config.get_or("QUEUE_TRIES", 3_u32).max(1),
            retry_after: config.get_or("QUEUE_RETRY_AFTER", 90),
            driver,
        })
    }

    /// Stores a job for `name`'s handler, to run as soon as a worker is free.
    ///
    /// # Errors
    ///
    /// [`Error::Unknown`] when no handler has that name; a store error when
    /// the database or Redis fails, so the caller decides what to do.
    pub fn dispatch(&self, name: &str, payload: &str) -> Result<(), Error> {
        self.dispatch_later(name, payload, Duration::ZERO)
    }

    /// [`dispatch`](Self::dispatch), but the job runs no sooner than `delay`
    /// from now (to the second).
    ///
    /// # Errors
    ///
    /// As [`dispatch`](Self::dispatch).
    pub fn dispatch_later(&self, name: &str, payload: &str, delay: Duration) -> Result<(), Error> {
        if !self.handlers.contains_key(name) {
            return Err(Error::Unknown(name.to_owned()));
        }
        let due = seconds(SystemTime::now()).saturating_add_unsigned(delay.as_secs());
        match &self.driver {
            Driver::Database => {
                self.db.with(|sql| {
                    sql.execute(
                        "INSERT INTO jobs (name, payload, available_at) VALUES (?1, ?2, ?3)",
                        params![name, payload, due],
                    )
                })?;
            }
            #[cfg(feature = "redis")]
            Driver::Redis { redis, prefix, .. } => {
                let job = format!("0 {} {name}\n{payload}", crate::uuid::Uuid::v4());
                if delay.is_zero() {
                    redis.command(&["RPUSH", &format!("{prefix}queue:jobs"), &job])?;
                } else {
                    let key = format!("{prefix}queue:delayed");
                    redis.command(&["ZADD", &key, &due.to_string(), &job])?;
                }
            }
        }
        Ok(())
    }

    /// Runs the next job that is due at `now` on this thread, if there is
    /// one, and returns whether there was. A failure is retried or moved to
    /// `failed_jobs`, not returned. Tests call it with their own clock.
    ///
    /// # Errors
    ///
    /// A store error.
    pub fn work_once(&self, now: SystemTime) -> Result<bool, Error> {
        let now = seconds(now);
        let Some(job) = self.reserve(now)? else {
            return Ok(false);
        };
        let outcome = match self.handlers.get(job.name.as_str()) {
            // A worker died during each attempt so far: stop retrying.
            _ if job.attempt > self.tries => Err(format!(
                "reserved {} times without finishing",
                job.attempt - 1
            )),
            None => Err(format!("no handler named {:?}", job.name)),
            Some(handler) => run(
                *handler,
                &Job {
                    name: &job.name,
                    payload: &job.payload,
                    attempt: job.attempt,
                    db: &self.db,
                },
            ),
        };
        match outcome {
            Ok(()) => self.remove(&job)?,
            Err(problem) if job.attempt < self.tries => {
                Log::warning(format_args!(
                    "queue: job {} failed (attempt {}), retrying: {problem}",
                    job.name, job.attempt
                ));
                let backoff = BACKOFF[(job.attempt as usize - 1).min(BACKOFF.len() - 1)];
                self.release(&job, now + backoff)?;
            }
            Err(problem) => {
                Log::error(format_args!(
                    "queue: job {} failed after {} attempts: {problem}",
                    job.name, job.attempt
                ));
                self.db.with(|sql| {
                    sql.execute(
                        "INSERT INTO failed_jobs (name, payload, error, failed_at) VALUES (?1, ?2, ?3, ?4)",
                        params![job.name, job.payload, problem, now],
                    )
                })?;
                self.remove(&job)?;
            }
        }
        Ok(true)
    }

    /// Runs jobs until `shutdown` is stopped, polling once a second while
    /// none is due. A job already running finishes first.
    pub fn work(&self, shutdown: &Shutdown) {
        while !shutdown.is_stopped() {
            match self.work_once(SystemTime::now()) {
                Ok(true) => {}
                Ok(false) => std::thread::sleep(Duration::from_secs(1)),
                Err(problem) => {
                    Log::error(format_args!("{problem}"));
                    std::thread::sleep(Duration::from_secs(10));
                }
            }
        }
    }

    /// Starts `workers` threads running [`work`](Self::work), each with its
    /// own Redis connection.
    pub fn start(&self, workers: usize, shutdown: &Shutdown) -> Vec<JoinHandle<()>> {
        (0..workers)
            .map(|_| {
                let (queue, shutdown) = (self.reconnected(), shutdown.clone());
                std::thread::spawn(move || queue.work(&shutdown))
            })
            .collect()
    }

    /// A clone with a connection of its own: the Redis client is one
    /// connection behind a lock.
    fn reconnected(&self) -> Self {
        #[cfg(feature = "redis")]
        if let Driver::Redis { url, prefix, .. } = &self.driver {
            return Self {
                driver: Driver::Redis {
                    redis: crate::redis::Redis::connect(url).expect("the URL parsed in Queue::new"),
                    url: url.clone(),
                    prefix: prefix.clone(),
                },
                ..self.clone()
            };
        }
        self.clone()
    }

    fn reserve(&self, now: i64) -> Result<Option<Reserved>, Error> {
        match &self.driver {
            Driver::Database => Ok(self.db.with(|sql| {
                sql.query_row(
                    "UPDATE jobs SET reserved_at = ?1, attempts = attempts + 1
                     WHERE id = (SELECT id FROM jobs WHERE available_at <= ?1
                                 AND (reserved_at IS NULL OR reserved_at <= ?2) ORDER BY id LIMIT 1)
                     RETURNING id, name, payload, attempts",
                    params![now, now - self.retry_after],
                    |row| {
                        Ok(Reserved {
                            token: row.get::<_, i64>(0)?.to_string(),
                            name: row.get(1)?,
                            payload: row.get(2)?,
                            attempt: row.get(3)?,
                        })
                    },
                )
                .optional()
            })?),
            #[cfg(feature = "redis")]
            Driver::Redis { redis, prefix, .. } => {
                use crate::redis::{Error as RedisError, Value};
                let key = |name: &str| format!("{prefix}queue:{name}");
                let reply = redis.command(&[
                    "EVAL",
                    RESERVE,
                    "3",
                    &key("jobs"),
                    &key("delayed"),
                    &key("reserved"),
                    &now.to_string(),
                    &self.retry_after.to_string(),
                ])?;
                let member = match reply {
                    Value::Nil => return Ok(None),
                    Value::Bytes(bytes) => String::from_utf8(bytes)
                        .map_err(|_| RedisError::Protocol("a job that is not UTF-8".into()))?,
                    other => {
                        return Err(
                            RedisError::Protocol(format!("unexpected reply {other:?}")).into()
                        );
                    }
                };
                let parsed = member.split_once('\n').and_then(|(header, payload)| {
                    let mut parts = header.splitn(3, ' ');
                    let attempt = parts.next()?.parse().ok()?;
                    let name = parts.nth(1)?.to_owned();
                    Some((attempt, name, payload.to_owned()))
                });
                let (attempt, name, payload) = parsed
                    .ok_or_else(|| RedisError::Protocol(format!("malformed job {member:?}")))?;
                Ok(Some(Reserved {
                    token: member,
                    name,
                    payload,
                    attempt,
                }))
            }
        }
    }

    /// Makes `job` available again at `due`.
    fn release(&self, job: &Reserved, due: i64) -> Result<(), Error> {
        match &self.driver {
            Driver::Database => {
                self.db.with(|sql| {
                    sql.execute(
                        "UPDATE jobs SET reserved_at = NULL, available_at = ?2 WHERE id = ?1",
                        params![row_id(job), due],
                    )
                })?;
            }
            #[cfg(feature = "redis")]
            Driver::Redis { redis, prefix, .. } => {
                // Delayed first: a crash in between runs the job twice, never zero times.
                let delayed = format!("{prefix}queue:delayed");
                redis.command(&["ZADD", &delayed, &due.to_string(), &job.token])?;
                redis.command(&["ZREM", &format!("{prefix}queue:reserved"), &job.token])?;
            }
        }
        Ok(())
    }

    /// Forgets `job`: done, or recorded in `failed_jobs`.
    fn remove(&self, job: &Reserved) -> Result<(), Error> {
        match &self.driver {
            Driver::Database => {
                self.db
                    .with(|sql| sql.execute("DELETE FROM jobs WHERE id = ?1", [row_id(job)]))?;
            }
            #[cfg(feature = "redis")]
            Driver::Redis { redis, prefix, .. } => {
                redis.command(&["ZREM", &format!("{prefix}queue:reserved"), &job.token])?;
            }
        }
        Ok(())
    }
}

/// How many worker threads `App::run` starts beside the HTTP server:
/// `QUEUE_WORKERS`, by default 1, or 0 when `APP_ENV=production`.
///
/// # Panics
///
/// When `QUEUE_WORKERS` is set but not a number.
pub fn workers(config: &Config) -> usize {
    config.get_or("QUEUE_WORKERS", usize::from(!config.is_production()))
}

/// Runs `handler`, turning a panic into a failure.
fn run(handler: Handler, job: &Job<'_>) -> Result<(), String> {
    std::panic::catch_unwind(AssertUnwindSafe(|| handler(job))).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(format!("panicked: {message}"))
    })
}

fn row_id(job: &Reserved) -> i64 {
    job.token
        .parse()
        .expect("a database job's token is its row id")
}

fn create_tables(sql: &crate::db::sqlite::Connection) -> crate::db::sqlite::Result<()> {
    sql.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            payload TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            available_at INTEGER NOT NULL,
            reserved_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS jobs_available ON jobs (available_at);
        CREATE TABLE IF NOT EXISTS failed_jobs (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            payload TEXT NOT NULL,
            error TEXT NOT NULL,
            failed_at INTEGER NOT NULL
        )",
    )
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

impl crate::web::Request {
    /// The app's [`Queue`].
    ///
    /// # Panics
    ///
    /// When the router has no `Queue` state ([`App`](crate::web::App) adds it).
    pub fn queue(&self) -> &Queue {
        self.state::<Queue>()
            .expect("no Queue: add .state(Queue::new(..)) to the router")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(env: &str, jobs: Vec<(&'static str, Handler)>) -> (Queue, Db) {
        let config = Config::parse(&format!("DB_DATABASE=:memory:\n{env}"));
        let db = Db::open(&config);
        (Queue::new(&config, db.clone(), jobs).unwrap(), db)
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|c| c.query_row(sql, [], |row| row.get(0))).unwrap()
    }

    fn record(job: &Job<'_>) -> Result<(), String> {
        job.db
            .with(|sql| {
                sql.execute_batch(
                    "CREATE TABLE IF NOT EXISTS done (payload TEXT, attempt INTEGER)",
                )?;
                sql.execute(
                    "INSERT INTO done VALUES (?1, ?2)",
                    params![job.payload, job.attempt],
                )
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_dispatched_job_runs_once() {
        let (queue, db) = queue("", vec![("record", record)]);
        queue.dispatch("record", r#"{"id":7}"#).unwrap();
        let now = SystemTime::now();
        assert!(queue.work_once(now).unwrap());
        assert!(!queue.work_once(now).unwrap(), "done jobs are gone");
        assert_eq!(count(&db, "SELECT COUNT(*) FROM jobs"), 0);
        let done: (String, u32) = db
            .with(|c| {
                c.query_row("SELECT payload, attempt FROM done", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
            })
            .unwrap();
        assert_eq!(done, (r#"{"id":7}"#.to_owned(), 1));
    }

    #[test]
    fn an_app_dispatches_from_a_handler_and_a_test_runs_the_job() {
        use crate::web::{App, Response, Router, testing::Client};
        fn routes(router: Router, _: &Config, _: &Db) -> Router {
            router.get("/", |request| {
                match request.queue().dispatch("record", "hi") {
                    Ok(()) => Response::text(202, "queued"),
                    Err(error) => Response::text(500, &error.to_string()),
                }
            })
        }
        let app = App {
            logging: crate::log::Settings::from_config,
            database: crate::db::Settings::from_config,
            filesystems: |_| crate::storage::Settings {
                default: "local".into(),
                disks: HashMap::from([("local".into(), crate::storage::Disk::new("storage"))]),
                links: Vec::new(),
            },
            migrations: Vec::new,
            seeders: Vec::new,
            routes,
            #[cfg(feature = "auth")]
            roles: &["super-admin", "user", "blocked"],
            #[cfg(feature = "mail")]
            mail: crate::mail::Settings::from_config,
            jobs: || vec![("record", record)],
        };
        let (router, db) = app.test("");
        assert_eq!(Client::new(&router).get("/").status, 202);
        let queue = app.queue(&Config::parse(""), db.clone());
        assert!(queue.work_once(SystemTime::now()).unwrap());
        assert_eq!(count(&db, "SELECT COUNT(*) FROM done"), 1);
    }

    #[test]
    fn a_delayed_job_waits() {
        let (queue, _db) = queue("", vec![("record", record)]);
        queue
            .dispatch_later("record", "x", Duration::from_secs(60))
            .unwrap();
        let now = SystemTime::now();
        assert!(!queue.work_once(now).unwrap());
        assert!(queue.work_once(now + Duration::from_secs(61)).unwrap());
    }

    #[test]
    fn unknown_names_are_refused_at_dispatch() {
        let (queue, db) = queue("", vec![("record", record)]);
        assert!(
            matches!(queue.dispatch("recrod", "x"), Err(Error::Unknown(name)) if name == "recrod")
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM jobs"), 0);
    }

    #[test]
    fn failures_back_off_then_land_in_failed_jobs() {
        let (queue, db) = queue("", vec![("flaky", |_| Err("upstream 503".into()))]);
        queue.dispatch("flaky", "p").unwrap();
        let start = SystemTime::now();
        let at = |s| start + Duration::from_secs(s);
        assert!(queue.work_once(at(0)).unwrap(), "attempt 1");
        assert!(!queue.work_once(at(9)).unwrap(), "10 s backoff");
        assert!(queue.work_once(at(10)).unwrap(), "attempt 2");
        assert!(!queue.work_once(at(69)).unwrap(), "60 s backoff");
        assert_eq!(count(&db, "SELECT COUNT(*) FROM failed_jobs"), 0);
        assert!(queue.work_once(at(70)).unwrap(), "attempt 3, the last");
        assert_eq!(count(&db, "SELECT COUNT(*) FROM jobs"), 0);
        let failed: (String, String, String) = db
            .with(|c| {
                c.query_row("SELECT name, payload, error FROM failed_jobs", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
            })
            .unwrap();
        assert_eq!(failed, ("flaky".into(), "p".into(), "upstream 503".into()));
    }

    #[test]
    fn a_panic_is_a_failure() {
        let (queue, db) = queue("QUEUE_TRIES=1\n", vec![("boom", |_| panic!("kaboom"))]);
        queue.dispatch("boom", "p").unwrap();
        assert!(queue.work_once(SystemTime::now()).unwrap());
        let error: String = db
            .with(|c| c.query_row("SELECT error FROM failed_jobs", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(error, "panicked: kaboom");
    }

    #[test]
    fn an_abandoned_reservation_runs_again_after_retry_after() {
        let (queue, db) = queue("", vec![("record", record)]);
        queue.dispatch("record", "x").unwrap();
        let now = seconds(SystemTime::now());
        // A worker reserves the job, then dies.
        assert!(queue.reserve(now).unwrap().is_some());
        let at = |s| UNIX_EPOCH + Duration::from_secs((now + s) as u64);
        assert!(!queue.work_once(at(89)).unwrap(), "still reserved");
        assert!(queue.work_once(at(90)).unwrap());
        assert_eq!(
            count(&db, "SELECT attempt FROM done"),
            2,
            "the dead run counts"
        );
    }

    #[test]
    fn workers_default_to_none_in_production() {
        assert_eq!(workers(&Config::parse("")), 1);
        assert_eq!(workers(&Config::parse("APP_ENV=production\n")), 0);
        assert_eq!(
            workers(&Config::parse("APP_ENV=production\nQUEUE_WORKERS=4\n")),
            4
        );
    }

    #[test]
    fn an_unknown_connection_is_refused() {
        let config = Config::parse("DB_DATABASE=:memory:\nQUEUE_CONNECTION=sqs\n");
        assert!(matches!(
            Queue::new(&config, Db::open(&config), Vec::new()),
            Err(Error::Config(_))
        ));
    }

    #[cfg(feature = "redis")]
    mod redis {
        use super::*;
        use crate::redis::tests::fake;

        #[test]
        fn production_needs_a_prefix() {
            let config =
                Config::parse("DB_DATABASE=:memory:\nQUEUE_CONNECTION=redis\nAPP_ENV=production\n");
            assert!(matches!(
                Queue::new(&config, Db::open(&config), Vec::new()),
                Err(Error::Config(why)) if why.contains("REDIS_PREFIX")
            ));
        }

        fn bulk(text: &str) -> &'static str {
            format!("${}\r\n{text}\r\n", text.len()).leak()
        }

        #[test]
        fn jobs_go_through_redis_and_fail_into_the_database() {
            let (url, server) = fake(vec![
                ":1\r\n",
                bulk("1 id-1 flaky\np"),
                ":1\r\n",
                ":1\r\n",
                bulk("3 id-1 flaky\np"),
                ":1\r\n",
                "$-1\r\n",
            ]);
            let (queue, db) = queue(
                &format!("QUEUE_CONNECTION=redis\nREDIS_URL={url}\nREDIS_PREFIX=app:\n"),
                vec![("flaky", |_| Err("nope".into()))],
            );
            queue.dispatch("flaky", "p").unwrap();
            let now = SystemTime::now();
            assert!(queue.work_once(now).unwrap(), "attempt 1, retried");
            assert!(queue.work_once(now).unwrap(), "attempt 3, failed");
            assert!(!queue.work_once(now).unwrap(), "empty");
            assert_eq!(count(&db, "SELECT COUNT(*) FROM failed_jobs"), 1);

            let seen = server.join().unwrap();
            let now = seconds(now).to_string();
            assert_eq!(seen[0][..2], ["RPUSH", "app:queue:jobs"]);
            assert!(seen[0][2].starts_with("0 ") && seen[0][2].ends_with(" flaky\np"));
            assert_eq!(seen[1][0], "EVAL");
            assert_eq!(
                seen[1][2..],
                [
                    "3",
                    "app:queue:jobs",
                    "app:queue:delayed",
                    "app:queue:reserved",
                    &now,
                    "90"
                ]
            );
            let due = (now.parse::<i64>().unwrap() + 10).to_string();
            assert_eq!(
                seen[2],
                ["ZADD", "app:queue:delayed", &due, "1 id-1 flaky\np"]
            );
            assert_eq!(seen[3], ["ZREM", "app:queue:reserved", "1 id-1 flaky\np"]);
            assert_eq!(seen[5], ["ZREM", "app:queue:reserved", "3 id-1 flaky\np"]);
        }

        /// Against a real server, in CI: `RUSTCLAMP_TEST_REDIS_URL`; skipped without it.
        #[test]
        fn a_real_redis() {
            let Ok(url) = std::env::var("RUSTCLAMP_TEST_REDIS_URL") else {
                return;
            };
            let prefix = format!("rustclamp-test-queue:{}:", std::process::id());
            let (queue, db) = queue(
                &format!("QUEUE_CONNECTION=redis\nREDIS_URL={url}\nREDIS_PREFIX={prefix}\n"),
                vec![("record", record)],
            );
            queue.dispatch("record", "now").unwrap();
            queue
                .dispatch_later("record", "later", Duration::from_secs(30))
                .unwrap();
            let now = SystemTime::now();
            assert!(queue.work_once(now).unwrap());
            assert!(!queue.work_once(now).unwrap(), "the delayed one waits");
            assert!(queue.work_once(now + Duration::from_secs(30)).unwrap());
            // A reservation that is never finished comes back after 90 s.
            queue.dispatch("record", "abandoned").unwrap();
            let now = SystemTime::now();
            assert!(queue.reserve(seconds(now)).unwrap().is_some());
            assert!(!queue.work_once(now + Duration::from_secs(89)).unwrap());
            assert!(queue.work_once(now + Duration::from_secs(90)).unwrap());
            let runs: Vec<(String, u32)> = db
                .with(|c| {
                    c.prepare("SELECT payload, attempt FROM done")?
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()
                })
                .unwrap();
            assert_eq!(
                runs,
                [
                    ("now".into(), 1),
                    ("later".into(), 1),
                    ("abandoned".into(), 2)
                ]
            );
        }
    }
}
