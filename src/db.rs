//! A SQLite or Postgres database, configured from `.env`.
//!
//! Enabled by the `db` feature, which `clamp init --web` turns on. SQLite is
//! compiled into the app, so there is no server to run. `DB_CONNECTION`
//! names the engine (`sqlite`, or `pgsql` with the `postgres` feature) and
//! `DB_DATABASE` the SQLite file, `storage/database.sqlite` by default, or
//! `:memory:`.
//!
//! Postgres reads `DATABASE_URL` (`postgres://user:pass@host:port/db?sslmode=..`)
//! or `DB_HOST`, `DB_PORT` (5432), `DB_DATABASE`, `DB_USERNAME`,
//! `DB_PASSWORD`, `DB_SSLMODE` (`disable`, `require` or `verify-full`, which
//! is the default for any host but this machine), `DB_SSLROOTCERT` (a CA
//! file) and `DB_POOL` (most connections, 10).
//!
//! The API names no engine: [`Db::with`] lends a [`Connection`] that takes
//! `?`/`?N` marks and [`Param`]s ([`params!`]), and rows come back as
//! [`Row`]s of [`Value`]s, so the same code runs on both (ADR 0034).
//!
//! A migration is a struct implementing [`Migration`], one per file, with
//! `up` and `down` SQL written by [`Schema`] or by hand. [`Db::table`] builds
//! queries; [`Db::with`] lends the connection for anything else.
//!
//! ```
//! use rustclamp::config::Config;
//! use rustclamp::db::{Db, Migration, Schema, Sql, params};
//!
//! struct CreatePosts;
//!
//! impl Migration for CreatePosts {
//!     fn name(&self) -> &'static str {
//!         "2026_09_29_000001_create_posts"
//!     }
//!     fn up(&self) -> Sql {
//!         Schema::create("posts", |table| {
//!             table.id();
//!             table.string("title");
//!             table.timestamps();
//!         })
//!     }
//!     fn down(&self) -> Sql {
//!         Schema::drop("posts")
//!     }
//! }
//!
//! let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
//! db.migrate(&[&CreatePosts]).unwrap();
//! db.table("posts").insert(&["title"], params!["Hello"]).unwrap();
//! let titles = db
//!     .table("posts")
//!     .where_eq("title", &"Hello")
//!     .get(|row| row.get::<String>("title"))
//!     .unwrap();
//! assert_eq!(titles, ["Hello"]);
//! ```

use std::sync::{Arc, Mutex, PoisonError};

use crate::config::Config;

mod connection;
mod dialect;
#[cfg(feature = "postgres")]
mod pool;
#[cfg(feature = "postgres")]
mod postgres;
mod query;
mod schema;
mod states;
#[cfg(test)]
mod testing;
pub mod timestamp;
pub use dialect::{Dialect, Sql};
pub use query::{Page, Query};
pub use schema::{Column, OnDelete, Schema, Table};
pub use states::{Change, States, Transition};

/// A list of [`Param`]s for a statement, of any types:
/// `params![title, 3, None::<String>]`.
#[doc(inline)]
pub use crate::__db_params as params;
use connection::Inner;
pub use connection::{ColumnIndex, Connection, Error, FromColumn, Param, Result, Row, Value};
/// `#[derive(Model)]` with `#[model(table = "posts")]`: reads each field
/// from the column of the same name. See [`Model`](trait@Model).
pub use rustclamp_macros::Model;

/// A shared database. Clones share the same connections. SQLite has one
/// writer behind a lock and, for a WAL file database, read-only connections
/// that [`Db::read`] (and a [`Query`]'s `get`, `first` and `count`) take, so
/// reads do not wait for the writer or for each other (#39). Postgres has a
/// pool of connections, which `read` shares with `with` (#121).
#[derive(Clone)]
pub struct Db {
    backend: Backend,
}

/// The engine behind a [`Db`].
#[derive(Clone)]
enum Backend {
    Sqlite {
        // `:memory:` relies on the single writer: every clone must see the
        // same in-memory database, so it gets no readers.
        writer: Arc<Mutex<rusqlite::Connection>>,
        readers: Option<Arc<Readers>>,
    },
    #[cfg(feature = "postgres")]
    Postgres(Arc<pool::Pool>),
}

/// Idle read-only connections to the same file, opened on demand.
struct Readers {
    path: String,
    // ponytail: grows to the peak number of concurrent readers and keeps them
    // all; cap it if an app with thousands of threads shows up.
    idle: Mutex<Vec<rusqlite::Connection>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

/// Where [`Db::blocking`]'s thread leaves its result for the future.
struct Slot<T> {
    result: Option<std::thread::Result<T>>,
    waker: Option<std::task::Waker>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            result: None,
            waker: None,
        }
    }
}

/// A read-only connection to `path` for [`Db::read`].
fn open_reader(path: &str) -> rusqlite::Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}

/// Why [`Db::try_connect`] could not open the database.
#[derive(Debug)]
pub enum DbError {
    /// `DB_CONNECTION` names an engine this build does not have: not `sqlite`,
    /// and not `pgsql` with the `postgres` feature.
    Engine(String),
    /// The database could not be opened or set up.
    Open {
        /// The SQLite file, or the Postgres host, port and database.
        path: String,
        /// What the engine said.
        source: Error,
    },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(engine) if engine == "pgsql" => write!(
                f,
                "config key DB_CONNECTION is pgsql; build with the `postgres` feature"
            ),
            Self::Engine(engine) => write!(
                f,
                "config key DB_CONNECTION is {engine}; only sqlite and pgsql are supported"
            ),
            Self::Open { path, source } => write!(f, "cannot open database {path}: {source}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(_) => None,
            Self::Open { source, .. } => Some(source),
        }
    }
}

/// The journal mode [`Db::try_connect_with`] leaves a file database in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Journal {
    /// Write-ahead log: readers keep working while a write is in progress.
    #[default]
    Wal,
    /// Whatever the file already uses; a new file gets SQLite's rollback
    /// journal. Nothing is written to the file to switch modes.
    Keep,
}

/// Which database to open. An app builds it in `app/config/database.rs`.
#[derive(Clone)]
pub struct Settings {
    /// The engine: `sqlite` or `pgsql`.
    pub connection: String,
    /// The SQLite file, or `:memory:`. Postgres names its database in `url`.
    pub database: String,
    /// Postgres only: `postgres://user:pass@host:port/db?sslmode=..`.
    pub url: Option<String>,
    /// Postgres only: the most connections the pool opens.
    pub pool: usize,
}

/// Without the URL, which carries the password.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("connection", &self.connection)
            .field("database", &self.database)
            .field("url", &self.url.as_ref().map(|_| "<redacted>"))
            .field("pool", &self.pool)
            .finish()
    }
}

impl Settings {
    /// `DB_CONNECTION` (`sqlite`) and `DB_DATABASE`
    /// (`storage/database.sqlite`); for `pgsql`, `DATABASE_URL` or the
    /// `DB_HOST`, `DB_PORT`, `DB_DATABASE`, `DB_USERNAME`, `DB_PASSWORD`,
    /// `DB_SSLMODE` and `DB_SSLROOTCERT` parts of it, and `DB_POOL`.
    pub fn from_config(config: &Config) -> Self {
        // ponytail: the same defaults as the web template's
        // `app/config/database.rs`, for apps without one
        let connection = config.get("DB_CONNECTION").unwrap_or("sqlite");
        let pgsql = connection == "pgsql";
        Self {
            connection: connection.into(),
            database: config
                .get("DB_DATABASE")
                .unwrap_or(if pgsql { "" } else { "storage/database.sqlite" })
                .into(),
            url: pgsql.then(|| postgres_url(config)),
            pool: config
                .get("DB_POOL")
                .and_then(|pool| pool.parse().ok())
                .unwrap_or(10),
        }
    }
}

/// `DATABASE_URL`, or the URL the `DB_*` parts make.
fn postgres_url(config: &Config) -> String {
    if let Some(url) = config.get("DATABASE_URL") {
        return url.to_owned();
    }
    let get = |key, default| config.get(key).unwrap_or(default);
    let mut url = format!(
        "postgres://{}:{}@{}:{}/{}",
        percent_encode(get("DB_USERNAME", "postgres")),
        percent_encode(get("DB_PASSWORD", "")),
        get("DB_HOST", "127.0.0.1"),
        get("DB_PORT", "5432"),
        percent_encode(get("DB_DATABASE", "")),
    );
    let mut separator = '?';
    for (key, name) in [("DB_SSLMODE", "sslmode"), ("DB_SSLROOTCERT", "sslrootcert")] {
        if let Some(value) = config.get(key) {
            url.push_str(&format!("{separator}{name}={}", percent_encode(value)));
            separator = '&';
        }
    }
    url
}

/// `text` with everything but unreserved characters as `%XX`.
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

impl Db {
    /// Opens the database named by `DB_CONNECTION` and `DB_DATABASE`; see
    /// [`Db::connect`].
    pub fn open(config: &Config) -> Self {
        Self::connect(&Settings::from_config(config))
    }

    /// Opens the database `settings` names, creating the file and its folder
    /// when missing.
    ///
    /// # Panics
    ///
    /// When the connection is not `sqlite` (or `pgsql` with the `postgres`
    /// feature) or the database cannot be opened:
    /// the app should stop at startup rather than run without its data. CLIs
    /// and services that exit with their own code use [`Db::try_connect`].
    pub fn connect(settings: &Settings) -> Self {
        // Only for an engine try_connect accepts: an unsupported one must stop
        // the app without leaving a `storage/` folder behind.
        if settings.connection == "sqlite"
            && settings.database != ":memory:"
            && let Some(folder) = std::path::Path::new(&settings.database).parent()
        {
            let _ = std::fs::create_dir_all(folder);
        }
        Self::try_connect(settings).unwrap_or_else(|error| panic!("{error}"))
    }

    /// Opens the database `settings` names, creating the file but not its
    /// folder. A file that is not a database is left untouched.
    ///
    /// # Errors
    ///
    /// [`DbError::Engine`] when the connection is not `sqlite` (or `pgsql` with
    /// the `postgres` feature),
    /// [`DbError::Open`] when the database cannot be opened.
    pub fn try_connect(settings: &Settings) -> Result<Self, DbError> {
        Self::try_connect_with(settings, Journal::Wal)
    }

    /// [`Db::try_connect`], choosing the journal mode: [`Journal::Keep`]
    /// leaves the file's own mode alone, so a file that uses the rollback
    /// journal is opened without rewriting its header. Postgres has no
    /// journal, and ignores it.
    ///
    /// # Errors
    ///
    /// As [`Db::try_connect`].
    pub fn try_connect_with(settings: &Settings, journal: Journal) -> Result<Self, DbError> {
        let engine = settings.connection.as_str();
        #[cfg(feature = "postgres")]
        if engine == "pgsql" {
            return Self::try_connect_postgres(settings);
        }
        if engine != "sqlite" {
            return Err(DbError::Engine(engine.to_owned()));
        }
        let path = settings.database.as_str();
        let connection = if path == ":memory:" {
            rusqlite::Connection::open_in_memory()
        } else {
            rusqlite::Connection::open(path).and_then(|connection| {
                let patience = std::time::Duration::from_secs(5);
                connection.busy_timeout(patience)?;
                // WAL lets readers work while a write is in progress. The
                // switch upgrades a shared lock to an exclusive one, and SQLite
                // answers BUSY at once there (deadlock avoidance) without
                // calling the busy handler, so racing first opens retry (#34).
                let deadline = std::time::Instant::now() + patience;
                if journal == Journal::Wal {
                    loop {
                        match connection.pragma_update(None, "journal_mode", "WAL") {
                            Err(rusqlite::Error::SqliteFailure(error, _))
                                if error.code == rusqlite::ErrorCode::DatabaseBusy
                                    && std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            result => break result?,
                        }
                    }
                }
                Ok(connection)
            })
        }
        .and_then(|connection| {
            connection.pragma_update(None, "foreign_keys", true)?;
            Ok(connection)
        })
        .map_err(|source| DbError::Open {
            path: path.to_owned(),
            source: source.into(),
        })?;
        // A reader on a rollback journal would block the writer; only WAL
        // lets them run side by side.
        let readers = (path != ":memory:" && journal == Journal::Wal).then(|| {
            Arc::new(Readers {
                path: path.to_owned(),
                idle: Mutex::new(Vec::new()),
            })
        });
        Ok(Self {
            backend: Backend::Sqlite {
                writer: Arc::new(Mutex::new(connection)),
                readers,
            },
        })
    }

    #[cfg(feature = "postgres")]
    fn try_connect_postgres(settings: &Settings) -> Result<Self, DbError> {
        let open = |path: &str, message: String| DbError::Open {
            path: path.to_owned(),
            source: Error::new(message),
        };
        let url = settings.url.as_deref().unwrap_or_default();
        let config =
            postgres::Config::parse_url(url).map_err(|message| open("DATABASE_URL", message))?;
        let target = format!("{}:{}/{}", config.host, config.port, config.database);
        let pool = pool::Pool::new(config, settings.pool).map_err(|source| DbError::Open {
            path: target,
            source,
        })?;
        Ok(Self {
            backend: Backend::Postgres(Arc::new(pool)),
        })
    }

    /// The SQL dialect of the engine behind this database.
    pub fn dialect(&self) -> Dialect {
        match self.backend {
            Backend::Sqlite { .. } => Dialect::Sqlite,
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => Dialect::Postgres,
        }
    }

    /// Runs `work` with the connection, holding it for the duration. On
    /// Postgres that is one pooled connection, and `work` gets a connection
    /// whose calls all fail when the pool has none to lend.
    pub fn with<T>(&self, work: impl FnOnce(&Connection<'_>) -> T) -> T {
        match &self.backend {
            Backend::Sqlite { .. } => work(&Connection(Inner::Sqlite(&self.writer()))),
            #[cfg(feature = "postgres")]
            Backend::Postgres(pool) => pool.with(work),
        }
    }

    /// The SQLite writer, locked.
    fn writer(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        let writer = match &self.backend {
            Backend::Sqlite { writer, .. } => writer,
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => unreachable!("Postgres has no single writer"),
        };
        // A panic in another request leaves the connection usable; SQLite
        // rolls back its unfinished transaction.
        writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` with a read-only connection, sharing the database with
    /// other readers and with the writer ([`Db::with`]) instead of waiting
    /// for them. It sees everything committed before it started, but not a
    /// transaction still open on the writer; a write through it fails. An
    /// in-memory database, or one opened with [`Journal::Keep`], has one
    /// connection, and this is [`Db::with`]; so is it on Postgres, where a
    /// write through it works too.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::Db;
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// let one: i64 = db.read(|sql| sql.query_row("SELECT 1", &[], |row| row.get(0))).unwrap();
    /// assert_eq!(one, 1);
    /// ```
    pub fn read<T>(&self, work: impl FnOnce(&Connection<'_>) -> T) -> T {
        let readers = match &self.backend {
            Backend::Sqlite { readers, .. } => readers,
            // MVCC: Postgres reads run beside writes on the same pool.
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => return self.with(work),
        };
        let Some(readers) = readers else {
            return self.with(work);
        };
        let idle = readers
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let connection = match idle {
            Some(connection) => connection,
            None => match open_reader(&readers.path) {
                Ok(connection) => connection,
                // ponytail: a reader that cannot open falls back to the writer
                // rather than failing a query the writer could answer.
                Err(_) => return self.with(work),
            },
        };
        let value = work(&Connection(Inner::Sqlite(&connection)));
        // A panic in `work` drops the connection instead of returning it.
        readers
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(connection);
        value
    }

    /// [`Db::with`] for async callers: runs `work` with the connection on its
    /// own thread and returns a future for its result, so the caller's
    /// executor thread never waits on the connection lock or on SQLite. The
    /// thread starts when this is called, like Tokio's `spawn_blocking`; the
    /// future works on any executor. A panic in `work` panics the awaiting task.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::Db;
    ///
    /// # fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    /// #     struct Wake(std::thread::Thread);
    /// #     impl std::task::Wake for Wake { fn wake(self: std::sync::Arc<Self>) { self.0.unpark() } }
    /// #     let waker = std::sync::Arc::new(Wake(std::thread::current())).into();
    /// #     let mut future = std::pin::pin!(future);
    /// #     loop {
    /// #         if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut std::task::Context::from_waker(&waker)) { return value }
    /// #         std::thread::park();
    /// #     }
    /// # }
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// let one: i64 = block_on(db.blocking(|sql| sql.query_row("SELECT 1", &[], |row| row.get(0)))).unwrap();
    /// assert_eq!(one, 1);
    /// ```
    pub fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection<'_>) -> T + Send + 'static,
    ) -> impl std::future::Future<Output = T> + Send + 'static {
        // ponytail: a thread per call, no pool: the pool waits for
        // measurements (ADR 0009); a bounded worker set replaces this if
        // thread spawn shows up.
        let slot = Arc::new(Mutex::new(Slot::default()));
        let (db, done) = (self.clone(), slot.clone());
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.with(work)));
            let mut slot = done.lock().unwrap_or_else(PoisonError::into_inner);
            slot.result = Some(result);
            if let Some(waker) = slot.waker.take() {
                waker.wake();
            }
        });
        std::future::poll_fn(move |cx| {
            let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
            match slot.result.take() {
                Some(Ok(value)) => std::task::Poll::Ready(value),
                Some(Err(panic)) => std::panic::resume_unwind(panic),
                None => {
                    slot.waker = Some(cx.waker().clone());
                    std::task::Poll::Pending
                }
            }
        })
    }

    /// Runs `work` in a transaction, like Laravel's `DB::transaction`: kept
    /// when it returns `Ok`, undone when it returns `Err` or panics. It
    /// nests: inside another transaction, such as a seeder's, it is a
    /// savepoint. `work` gets a [`Tx`]: query through it (`tx.table(..)`, or
    /// the [`Connection`] it derefs to), not through this `Db`, whose lock it
    /// holds; a `Db` write inside `work` waits forever, and a `Db` read sees
    /// only what was committed before it.
    ///
    /// On SQLite the write lock is taken at the first write, so two
    /// transactions that both read and then write can meet a "database is
    /// locked" error; use [`Db::transaction_immediate`] for those. On Postgres
    /// the transaction holds one pooled connection until it ends, and `Db`
    /// calls `work` makes on the same thread share it, but a spawned thread, or
    /// [`Db::blocking`], gets a connection of its own, outside the transaction.
    /// Postgres refuses every statement after a failed one, so if `work` ignores
    /// an error and returns `Ok`, the transaction is rolled back and the call
    /// returns an error; SQLite would keep the rest.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{self, Db};
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch("CREATE TABLE t (n INTEGER)")).unwrap();
    /// let failed: db::Result<()> = db.transaction(|tx| {
    ///     tx.table("t").insert(&["n"], &[&1])?;
    ///     tx.execute("NOT SQL", &[])?;
    ///     Ok(())
    /// });
    /// assert!(failed.is_err());
    /// assert_eq!(db.table("t").count().unwrap(), 0, "the insert was undone");
    /// ```
    pub fn transaction<T, E: From<Error>>(
        &self,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.run_transaction(false, work)
    }

    /// [`Db::transaction`], but it takes the write lock up front (`BEGIN
    /// IMMEDIATE`), waiting out the busy timeout, so a read-then-write
    /// cannot fail halfway on a lock another process took meanwhile. Inside
    /// another transaction it is a savepoint of that one, which keeps
    /// whatever lock that one has. On Postgres it takes an advisory lock
    /// right after `BEGIN`, so one such transaction runs at a time across
    /// every instance of the app.
    pub fn transaction_immediate<T, E: From<Error>>(
        &self,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.run_transaction(true, work)
    }

    fn run_transaction<T, E: From<Error>>(
        &self,
        immediate: bool,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        #[cfg(feature = "postgres")]
        if let Backend::Postgres(pool) = &self.backend {
            return postgres_transaction(pool, immediate, work);
        }
        let behavior = if immediate {
            rusqlite::TransactionBehavior::Immediate
        } else {
            rusqlite::TransactionBehavior::Deferred
        };
        let mut connection = self.writer();
        // rusqlite's Transaction and Savepoint roll back when dropped, so an
        // `Err` or a panic in `work` leaves no transaction open behind it.
        if connection.is_autocommit() {
            let transaction = connection
                .transaction_with_behavior(behavior)
                .map_err(Error::from)?;
            let value = work(&Tx(Connection(Inner::Sqlite(&transaction))))?;
            transaction.commit().map_err(Error::from)?;
            Ok(value)
        } else {
            let savepoint = connection.savepoint().map_err(Error::from)?;
            let value = work(&Tx(Connection(Inner::Sqlite(&savepoint))))?;
            savepoint.commit().map_err(Error::from)?;
            Ok(value)
        }
    }

    /// A query on `table`; see [`Query`].
    pub fn table<'a>(&'a self, table: &str) -> Query<'a> {
        Query::new(self, table)
    }

    /// Runs `up` for each migration not yet run, in order, as one batch,
    /// recording each in the `migrations` table. A migration that fails is
    /// rolled back and stops the rest.
    ///
    /// On Postgres the whole run holds an advisory lock, so instances that
    /// migrate at startup take turns.
    pub fn migrate(&self, migrations: &[&dyn Migration]) -> Result<()> {
        self.with(|connection| {
            let postgres = connection.dialect() == Dialect::Postgres;
            if postgres {
                connection.execute_batch(&format!("SELECT pg_advisory_lock({MIGRATE_LOCK})"))?;
            }
            let result = Self::migrate_on(connection, migrations);
            if postgres {
                // Best effort: a broken connection took its session lock with it.
                let _ =
                    connection.execute_batch(&format!("SELECT pg_advisory_unlock({MIGRATE_LOCK})"));
            }
            result
        })
    }

    fn migrate_on(connection: &Connection<'_>, migrations: &[&dyn Migration]) -> Result<()> {
        {
            connection.execute_batch(match connection.dialect() {
                Dialect::Sqlite => {
                    "CREATE TABLE IF NOT EXISTS migrations (
                    name TEXT PRIMARY KEY,
                    batch INTEGER NOT NULL,
                    ran_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                )"
                }
                Dialect::Postgres => {
                    "CREATE TABLE IF NOT EXISTS migrations (
                    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE,
                    batch BIGINT NOT NULL,
                    ran_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
                )"
                }
            })?;
            let batch: i64 = connection.query_row(
                "SELECT coalesce(max(batch), 0) + 1 FROM migrations",
                &[],
                |row| row.get(0),
            )?;
            for migration in migrations {
                let name = migration.name();
                let ran: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM migrations WHERE name = ?1)",
                    &[&name],
                    |row| row.get(0),
                )?;
                if ran {
                    continue;
                }
                atomically(connection, |connection| {
                    connection.execute_batch(migration.up().render(connection.dialect()))?;
                    connection.execute(
                        "INSERT INTO migrations (name, batch) VALUES (?1, ?2)",
                        params![name, batch],
                    )
                })?;
            }
            Ok(())
        }
    }

    /// [`Db::migrate`] for a database that tracks its schema in SQLite's
    /// `PRAGMA user_version` instead of the `migrations` table: the number is
    /// how many of `migrations` have run, so the list may only grow at the end
    /// and there is no batch or rollback. Each pending migration runs in a
    /// transaction together with its version bump; a failure stops the rest.
    ///
    /// `baseline` adopts a database created before this: when its
    /// `user_version` is 0 but it already has tables, the first `baseline`
    /// migrations are taken as already applied. A new empty database runs
    /// everything.
    ///
    /// # Errors
    ///
    /// When SQL fails, the database is at a version beyond `migrations`
    /// (it was migrated by a newer build), or the engine is Postgres, which
    /// has no `user_version`.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, Migration, Sql};
    ///
    /// struct Raw(&'static str, &'static str);
    /// impl Migration for Raw {
    ///     fn name(&self) -> &'static str { self.0 }
    ///     fn up(&self) -> Sql { self.1.into() }
    ///     fn down(&self) -> Sql { "".into() }
    /// }
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// // An existing database that already has the first table.
    /// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY)")).unwrap();
    /// let first = Raw("0001", "CREATE TABLE posts (id INTEGER PRIMARY KEY);");
    /// let second = Raw("0002", "CREATE TABLE tags (id INTEGER PRIMARY KEY);");
    /// db.migrate_user_version(1, &[&first, &second]).unwrap();
    /// assert_eq!(db.with(|sql| sql.query_row("PRAGMA user_version", &[], |r| r.get::<i64>(0))).unwrap(), 2);
    /// ```
    pub fn migrate_user_version(
        &self,
        baseline: usize,
        migrations: &[&dyn Migration],
    ) -> Result<()> {
        self.with(|connection| {
            if connection.dialect() == Dialect::Postgres {
                return Err(Error::new(
                    "migrate_user_version is SQLite only; use migrate on Postgres",
                ));
            }
            let set_version = |version: i64| {
                connection.execute_batch(&format!("PRAGMA user_version = {version}"))
            };
            let mut current: i64 =
                connection.query_row("PRAGMA user_version", &[], |row| row.get(0))?;
            if current == 0 && baseline > 0 {
                let has_tables: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
                    &[],
                    |row| row.get(0),
                )?;
                if has_tables {
                    // `baseline` is a count of migrations, never near i64::MAX.
                    current = baseline as i64;
                    set_version(current)?;
                }
            }
            if current < 0 || current as usize > migrations.len() {
                return Err(Error::new(format!(
                    "database user_version {current} is beyond the {} known migrations",
                    migrations.len()
                )));
            }
            for (index, migration) in migrations.iter().enumerate().skip(current as usize) {
                savepoint(connection, |connection| {
                    connection.execute_batch(migration.up().render(connection.dialect()))?;
                    set_version(index as i64 + 1)
                })?;
            }
            Ok(())
        })
    }

    /// Each migration's name and the batch it ran in, or `None` while
    /// pending, in the order given.
    pub fn status(
        &self,
        migrations: &[&dyn Migration],
    ) -> Result<Vec<(&'static str, Option<i64>)>, String> {
        let ran: Vec<(String, i64)> = self
            .with(|connection| {
                let exists: bool = connection.query_row(
                    match connection.dialect() {
                        Dialect::Sqlite => {
                            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'migrations')"
                        }
                        Dialect::Postgres => {
                            "SELECT EXISTS (SELECT 1 FROM information_schema.tables
                             WHERE table_schema = current_schema() AND table_name = 'migrations')"
                        }
                    },
                    &[],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(Vec::new());
                }
                connection.query("SELECT name, batch FROM migrations", &[], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
            })
            .map_err(|error: Error| error.to_string())?;
        Ok(migrations
            .iter()
            .map(|migration| {
                let batch = ran
                    .iter()
                    .find(|(name, _)| name == migration.name())
                    .map(|(_, batch)| *batch);
                (migration.name(), batch)
            })
            .collect())
    }

    /// Runs each seeder in order, each in its own transaction, so a failing
    /// seeder leaves nothing half inserted.
    pub fn seed(&self, seeders: &[&dyn Seeder]) -> Result<(), SeedError> {
        // ponytail: the transaction spans several `with` calls on the shared
        // connection, so run seeders from the console, not while serving.
        // On Postgres the seeder's `Db` calls on this thread share the
        // transaction's client; calls from other threads do not.
        for seeder in seeders {
            #[cfg(feature = "postgres")]
            if let Backend::Postgres(pool) = &self.backend {
                pool.pinned(|connection, _| {
                    connection.execute_batch("BEGIN")?;
                    end_transaction(connection, seeder.run(self), "COMMIT", "ROLLBACK")
                })??;
                continue;
            }
            self.with(|connection| connection.execute_batch("BEGIN"))?;
            match seeder.run(self) {
                Ok(()) => self.with(|connection| connection.execute_batch("COMMIT"))?,
                Err(error) => {
                    self.with(|connection| connection.execute_batch("ROLLBACK"))?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Runs `down` for every migration in the last batch, newest first, and
    /// returns their names. Each one is undone in its own transaction.
    ///
    /// # Errors
    ///
    /// When SQL fails, or a migration in the batch is missing from
    /// `migrations`, which would leave its `down` unknown.
    pub fn rollback(&self, migrations: &[&dyn Migration]) -> Result<Vec<&'static str>, String> {
        self.with(|connection| {
            let newest_first = match connection.dialect() {
                Dialect::Sqlite => "rowid",
                Dialect::Postgres => "id",
            };
            let names: Vec<String> = connection
                .query(
                    &format!(
                        "SELECT name FROM migrations
                         WHERE batch = (SELECT max(batch) FROM migrations)
                         ORDER BY {newest_first} DESC"
                    ),
                    &[],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let mut undone = Vec::new();
            for name in names {
                let migration = migrations
                    .iter()
                    .find(|migration| migration.name() == name)
                    .ok_or_else(|| format!("migration {name} ran but is not in the list"))?;
                atomically(connection, |connection| {
                    connection.execute_batch(migration.down().render(connection.dialect()))?;
                    connection.execute("DELETE FROM migrations WHERE name = ?1", &[&name])
                })
                .map_err(|error: Error| format!("{name}: {error}"))?;
                undone.push(migration.name());
            }
            Ok(undone)
        })
    }
}

/// The connection inside [`Db::transaction`]. It derefs to [`Connection`]
/// for raw SQL, and [`Tx::table`] builds queries that run in the transaction.
pub struct Tx<'a>(Connection<'a>);

impl<'a> Tx<'a> {
    /// A query on `table` inside this transaction; see [`Query`].
    pub fn table(&self, table: &str) -> Query<'a> {
        Query::in_transaction(self.0, table)
    }
}

impl<'a> std::ops::Deref for Tx<'a> {
    type Target = Connection<'a>;

    fn deref(&self) -> &Connection<'a> {
        &self.0
    }
}

/// A table's rows as a Rust type, like a Laravel model: name the table and
/// say how a row becomes `Self` once, then query with the builder.
/// `#[derive(Model)]` does both, reading each field from the column of the
/// same name.
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, Model, params};
///
/// #[derive(Model)]
/// #[model(table = "posts")]
/// struct Post {
///     id: i64,
///     title: String,
///     published_at: Option<String>,
/// }
///
/// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
/// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, published_at TEXT)"))
///     .unwrap();
/// let id = Post::query(&db).insert(&["title"], params!["Hello"]).unwrap();
/// assert_eq!(Post::find(&db, id).unwrap().unwrap().title, "Hello");
/// let titles: Vec<Post> = Post::query(&db).order_by_desc("id").get(Post::from_row).unwrap();
/// assert_eq!(titles[0].id, id);
/// assert_eq!(Post::all(&db).unwrap().len(), 1);
/// assert_eq!(Post::all(&db).unwrap()[0].published_at, None);
/// ```
///
/// Relations are queries, not a new type: `has_many` is a `where_eq` on the
/// foreign key, `belongs_to` is [`find`](Model::find), and loading the
/// owners of a list at once (no query per row) is a `where_in`:
///
/// ```
/// # use rustclamp::config::Config;
/// # use rustclamp::db::{Db, Model, Param};
/// #[derive(Model)]
/// #[model(table = "comments")]
/// struct Comment { id: i64, post_id: i64, body: String }
/// #[derive(Model)]
/// #[model(table = "posts")]
/// struct Post { id: i64, title: String }
///
/// # let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
/// # db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT);
/// #     CREATE TABLE comments (id INTEGER PRIMARY KEY, post_id INTEGER, body TEXT);
/// #     INSERT INTO posts VALUES (1, 'a'), (2, 'b');
/// #     INSERT INTO comments VALUES (1, 1, 'x'), (2, 1, 'y'), (3, 2, 'z');")).unwrap();
/// // has_many
/// let comments = Comment::query(&db).where_eq("post_id", &1).get(Comment::from_row).unwrap();
/// assert_eq!(comments.len(), 2);
/// // belongs_to
/// let post = Post::find(&db, comments[0].post_id).unwrap().unwrap();
/// assert_eq!(post.title, "a");
/// // eager: every comment's post in one query
/// let all = Comment::all(&db).unwrap();
/// let ids: Vec<&dyn Param> = all.iter().map(|c| &c.post_id as &dyn Param).collect();
/// let posts = Post::query(&db).where_in("id", &ids).get(Post::from_row).unwrap();
/// assert_eq!(posts.len(), 2);
/// ```
///
/// By hand, when a field is not a column of the same name:
///
/// ```
/// use rustclamp::db::{Model, Result, Row};
///
/// struct Tag {
///     label: String,
/// }
///
/// impl Model for Tag {
///     const TABLE: &'static str = "tags";
///
///     fn from_row(row: &Row) -> Result<Self> {
///         Ok(Self { label: row.get("name")? })
///     }
/// }
/// ```
///
/// The derive names its table, and needs named fields:
///
/// ```compile_fail
/// #[derive(rustclamp::db::Model)]
/// struct Post {
///     id: i64,
/// }
/// ```
///
/// ```compile_fail
/// #[derive(rustclamp::db::Model)]
/// #[model(table = "points")]
/// struct Point(i64, i64);
/// ```
pub trait Model: Sized {
    /// The table the rows live in.
    const TABLE: &'static str;

    /// Builds `Self` from one row, reading columns by name.
    fn from_row(row: &Row) -> Result<Self>;

    /// A query on [`TABLE`](Self::TABLE); finish it with
    /// `.get(Self::from_row)` or `.first(Self::from_row)`.
    fn query(db: &Db) -> Query<'_> {
        db.table(Self::TABLE)
    }

    /// Every row.
    fn all(db: &Db) -> Result<Vec<Self>> {
        Self::query(db).get(Self::from_row)
    }

    /// The row whose `id` is `id`.
    fn find(db: &Db, id: i64) -> Result<Option<Self>> {
        Self::query(db).where_eq("id", &id).first(Self::from_row)
    }

    /// The row whose `public_id` column is `public_id`, the UUID that goes in
    /// URLs so `id` never leaves the app. `None` for text that is not a UUID.
    fn find_public(db: &Db, public_id: &str) -> Result<Option<Self>> {
        let Some(uuid) = crate::uuid::Uuid::parse(public_id) else {
            return Ok(None);
        };
        Self::query(db)
            .where_eq("public_id", &uuid)
            .first(Self::from_row)
    }
}

/// Fills the database with data, like a Laravel seeder class: one struct
/// per file in `app/database/seeders/`, run by `cargo run -- db:seed`.
pub trait Seeder {
    /// Inserts the data, usually with [`Db::table`]. Any error stops the
    /// seeder and rolls back what it inserted; `?` works on database errors,
    /// [`Transition`]s and I/O alike.
    fn run(&self, db: &Db) -> Result<(), SeedError>;
}

/// Whatever stopped a [`Seeder`].
pub type SeedError = Box<dyn std::error::Error + Send + Sync>;

/// A migration under its file name. [`build`](crate::build) lists migrations
/// this way, so they need no `name()`.
#[doc(hidden)]
pub struct Named(pub &'static str, pub &'static dyn Migration);

impl Migration for Named {
    fn name(&self) -> &'static str {
        self.0
    }
    fn up(&self) -> Sql {
        self.1.up()
    }
    fn down(&self) -> Sql {
        self.1.down()
    }
}

/// Runs a database console command and returns the process exit code:
/// `migrate`, `migrate:rollback`, `migrate:status` or `db:seed`. The web template's `main`
/// calls it when the app gets an argument: `cargo run -- db:seed`.
pub fn command(
    db: &Db,
    command: &str,
    migrations: &[&dyn Migration],
    seeders: &[&dyn Seeder],
) -> i32 {
    let result = match command {
        "migrate" => db
            .migrate(migrations)
            .map_err(|error| error.to_string())
            .map(|()| {
                println!("Migrated");
            }),
        "migrate:status" => db.status(migrations).map(|rows| {
            for (name, batch) in rows {
                match batch {
                    Some(batch) => println!("Ran      {name} (batch {batch})"),
                    None => println!("Pending  {name}"),
                }
            }
        }),
        "migrate:rollback" => db.rollback(migrations).map(|names| {
            names.iter().for_each(|name| println!("Rolled back {name}"));
        }),
        "db:seed" => db
            .migrate(migrations)
            .map_err(|error| error.to_string())
            .and_then(|()| db.seed(seeders).map_err(|error| error.to_string()))
            .map(|()| println!("Seeded")),
        _ => {
            eprintln!(
                "unknown command {command:?}; try migrate, migrate:rollback, migrate:status or db:seed"
            );
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{command} failed: {error}");
            1
        }
    }
}

#[cfg(feature = "web")]
impl crate::web::Request {
    /// The app's database, added with `Router::state(db)`.
    ///
    /// # Panics
    ///
    /// When the router has no [`Db`] state: that is a wiring bug in the app.
    pub fn db(&self) -> &Db {
        self.state::<Db>()
            .expect("no database: add .state(db) to the router")
    }

    /// The `M` whose `public_id` is the route parameter `param`, like
    /// Laravel's route model binding; `None` answers 404:
    ///
    /// ```ignore
    /// fn show(request: &Request) -> rustclamp::web::Result {
    ///     let Some(post) = request.model::<Post>("post")? else {
    ///         return Ok(error(404));
    ///     };
    ///     Ok(request.render("posts/show", &[("post", &post)]))
    /// }
    /// ```
    ///
    /// # Panics
    ///
    /// As [`db`](Self::db).
    pub fn model<M: Model>(&self, param: &str) -> Result<Option<M>> {
        match self.param(param) {
            Some(public_id) => M::find_public(self.db(), public_id),
            None => Ok(None),
        }
    }
}

/// Runs `work` inside a savepoint: a transaction of its own, or a nested one
/// inside a transaction already open.
fn savepoint<T, E: From<Error>>(
    connection: &Connection<'_>,
    work: impl FnOnce(&Connection<'_>) -> Result<T, E>,
) -> Result<T, E> {
    connection.execute_batch("SAVEPOINT clamp")?;
    match work(connection) {
        Ok(value) => {
            connection.execute_batch("RELEASE clamp")?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK TO clamp; RELEASE clamp");
            Err(error)
        }
    }
}

/// Runs `work` as one transaction on a connection that is not in one: a
/// savepoint on SQLite, where that starts a transaction, `BEGIN` on Postgres,
/// where a savepoint outside a transaction is an error.
fn atomically<T, E: From<Error>>(
    connection: &Connection<'_>,
    work: impl FnOnce(&Connection<'_>) -> Result<T, E>,
) -> Result<T, E> {
    if connection.dialect() == Dialect::Sqlite {
        return savepoint(connection, work);
    }
    let (begin, commit, abort) = postgres_scope(connection);
    connection.execute_batch(begin)?;
    end_transaction(connection, work(connection), commit, abort)
}

/// How to open and end a Postgres transaction here: `BEGIN` on a connection
/// that is not in one (a `Db` call can nest inside a `with` that is not), a
/// savepoint inside one.
fn postgres_scope(connection: &Connection<'_>) -> (&'static str, &'static str, &'static str) {
    if connection.in_transaction() {
        (
            "SAVEPOINT clamp",
            "RELEASE clamp",
            "ROLLBACK TO clamp; RELEASE clamp",
        )
    } else {
        ("BEGIN", "COMMIT", "ROLLBACK")
    }
}

/// Ends a Postgres transaction (or savepoint) with `commit` when `result` is
/// `Ok`, else with `abort`. An `Ok` after an error `work` caught and ignored
/// is not kept: the server refuses everything in a failed transaction, and a
/// `COMMIT` would roll back without an error.
fn end_transaction<T, E: From<Error>>(
    connection: &Connection<'_>,
    result: Result<T, E>,
    commit: &str,
    abort: &str,
) -> Result<T, E> {
    match result {
        Ok(_) if connection.aborted() => {
            let _ = connection.execute_batch(abort);
            Err(Error::with_sqlstate(
                "the transaction was rolled back: a statement in it failed earlier",
                "25P02",
            )
            .into())
        }
        Ok(value) => {
            connection.execute_batch(commit)?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch(abort);
            Err(error)
        }
    }
}

/// [`Db::transaction`] on a pooled Postgres connection: `BEGIN`, `COMMIT` on
/// `Ok`, `ROLLBACK` on `Err`; a panic drops the connection, which rolls back.
/// The client is shared with the `Db` calls `work` makes on this thread, and
/// a transaction started inside one (a seeder's) is a savepoint, as on SQLite.
#[cfg(feature = "postgres")]
fn postgres_transaction<T, E: From<Error>>(
    pool: &pool::Pool,
    immediate: bool,
    work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
) -> Result<T, E> {
    pool.pinned(|connection, _| {
        let (begin, commit, abort) = postgres_scope(connection);
        connection.execute_batch(begin)?;
        let tx = Tx(*connection);
        let result = (|| {
            if immediate {
                tx.execute_batch(&format!("SELECT pg_advisory_xact_lock({IMMEDIATE_LOCK})"))?;
            }
            work(&tx)
        })();
        end_transaction(connection, result, commit, abort)
    })?
}

// ponytail: the keys are per database, not per schema, so two apps sharing
// one Postgres database also serialize each other's migrations and immediate
// transactions; derive them from `current_schema()` if that shows up. The
// wire client's 30 s read timeout also bounds one migration statement and the
// wait for `pg_advisory_lock`.
/// The advisory lock keys (`pg_advisory_lock`) that [`Db::migrate`] and
/// [`Db::transaction_immediate`] take on Postgres: arbitrary, fixed, and
/// different, so that an app's own transactions never wait for a migration.
const MIGRATE_LOCK: i64 = 0x636c_616d_7000_0001;
#[cfg(feature = "postgres")]
const IMMEDIATE_LOCK: i64 = 0x636c_616d_7000_0002;

/// The migration name for a source file: `file!()` without its folders and
/// `.rs`. Keeps a migration's recorded name equal to its file name, so a
/// file named `2026_09_29_000001_create_posts.rs` (loaded with
/// `#[path = "..."] mod create_posts;`, since a module name cannot start with
/// a digit) records `2026_09_29_000001_create_posts`.
///
/// ```
/// use rustclamp::db::migration_name;
///
/// assert_eq!(
///     migration_name("app/database/migrations/2026_09_29_000001_create_posts.rs"),
///     "2026_09_29_000001_create_posts"
/// );
/// assert_eq!(migration_name(r"app\database\0002_tags.rs"), "0002_tags");
/// ```
pub fn migration_name(file: &'static str) -> &'static str {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    base.strip_suffix(".rs").unwrap_or(base)
}

/// One change to the database schema, like a Laravel migration class. Its
/// name orders it and is recorded once it has run, so never rename or edit a
/// migration that has run anywhere: add a new one.
pub trait Migration {
    /// A unique name that sorts in run order, such as
    /// `2026_09_29_000001_create_posts`. Migrations listed by
    /// [`build::database`](crate::build) are named after their file, so they
    /// leave this out; one listed by hand implements it, usually as
    /// `migration_name(file!())`.
    ///
    /// # Panics
    ///
    /// When neither applies. Guessing would be worse: a name that later
    /// changes makes the migration run again.
    fn name(&self) -> &'static str {
        panic!(
            "migration {} has no name: list it with rustclamp::build::database or implement name()",
            std::any::type_name::<Self>()
        )
    }
    /// The SQL that applies the change, often [`Schema::create`].
    fn up(&self) -> Sql;
    /// The SQL that undoes [`up`](Self::up), often [`Schema::drop`].
    fn down(&self) -> Sql;
}

#[cfg(test)]
mod tests {
    use super::testing::{each_engine, is_postgres};
    use super::*;

    #[cfg(feature = "web")]
    #[test]
    fn model_binds_by_public_id() {
        each_engine(|db| model_binds_by_public_id_on(db.clone()));
    }

    #[cfg(feature = "web")]
    fn model_binds_by_public_id_on(db: Db) {
        use crate::web::{Request, Router, error};
        struct Post {
            title: String,
        }
        impl Model for Post {
            const TABLE: &'static str = "posts";
            fn from_row(row: &Row) -> Result<Self> {
                Ok(Self {
                    title: row.get("title")?,
                })
            }
        }
        let public_id = crate::uuid::Uuid::v7();
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, public_id TEXT, title TEXT)",
            )?;
            sql.execute(
                "INSERT INTO posts (id, public_id, title) VALUES (7, ?1, 'Hi')",
                &[&public_id],
            )
        })
        .unwrap();
        let app = Router::new()
            .state(db)
            .get("/posts/{post}", |request: &Request| {
                let Some(post) = request.model::<Post>("post")? else {
                    return Ok(error(404));
                };
                crate::web::Result::Ok(crate::web::Response::text(200, &post.title))
            });
        assert_eq!(
            app.handle(&Request::get(&format!("/posts/{public_id}")))
                .body,
            b"Hi"
        );
        // The internal id and non-UUID text are both just "not found".
        assert_eq!(app.handle(&Request::get("/posts/7")).status, 404);
        let other = crate::uuid::Uuid::v7();
        assert_eq!(
            app.handle(&Request::get(&format!("/posts/{other}"))).status,
            404
        );
    }

    #[test]
    fn a_database_speaks_its_engines_dialect() {
        let db = memory();
        assert_eq!(db.dialect(), Dialect::Sqlite);
        assert_eq!(db.with(|sql| sql.dialect()), Dialect::Sqlite);
        each_engine(|db| {
            let expected = if is_postgres(db) {
                Dialect::Postgres
            } else {
                Dialect::Sqlite
            };
            assert_eq!(db.dialect(), expected);
            assert_eq!(db.with(|sql| sql.dialect()), expected);
        });
    }

    /// A migration from literal SQL.
    struct Raw(&'static str, &'static str, &'static str);

    impl Migration for Raw {
        fn name(&self) -> &'static str {
            self.0
        }
        fn up(&self) -> Sql {
            self.1.into()
        }
        fn down(&self) -> Sql {
            self.2.into()
        }
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|connection| connection.query_row(sql, &[], |row| row.get(0)))
            .unwrap()
    }

    fn memory() -> Db {
        Db::open(&Config::parse("DB_DATABASE=:memory:"))
    }

    /// How many tables besides `migrations` the database has.
    fn tables(db: &Db) -> i64 {
        count(
            db,
            if is_postgres(db) {
                "SELECT count(*) FROM information_schema.tables
                 WHERE table_schema = current_schema() AND table_name != 'migrations'"
            } else {
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name != 'migrations'"
            },
        )
    }

    /// Polls `future` on this thread, parking until its waker fires.
    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Arc::new(Wake(std::thread::current())).into();
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(value) = future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&waker))
            {
                return value;
            }
            std::thread::park();
        }
    }

    #[test]
    fn blocking_runs_off_the_calling_thread_and_shares_the_database() {
        each_engine(blocking_runs_off_the_calling_thread);
    }

    fn blocking_runs_off_the_calling_thread(db: &Db) {
        let caller = std::thread::current().id();
        let (ran_on, one) = block_on(db.blocking(|sql| {
            sql.execute_batch("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (1)")
                .unwrap();
            (std::thread::current().id(), 1)
        }));
        assert_ne!(ran_on, caller);
        assert_eq!(one, 1);
        assert_eq!(count(db, "SELECT count(*) FROM t"), 1, "same database");
        // Many at once all complete.
        let all: Vec<_> = (0..8)
            .map(|n| db.blocking(move |sql| sql.execute("INSERT INTO t VALUES (?1)", &[&n])))
            .collect();
        for one in all {
            assert_eq!(block_on(one).unwrap(), 1);
        }
        assert_eq!(count(db, "SELECT count(*) FROM t"), 9);
    }

    #[test]
    fn blocking_panics_reach_the_awaiter_and_leave_the_db_usable() {
        each_engine(|db| {
            let caught = std::panic::catch_unwind(|| block_on(db.blocking(|_| panic!("boom"))));
            assert!(caught.is_err());
            assert_eq!(count(db, "SELECT 1"), 1);
        });
    }

    #[test]
    fn user_version_runs_pending_and_adopts_existing() {
        let a = Raw("0001", "CREATE TABLE a (n INTEGER)", "");
        let b = Raw("0002", "CREATE TABLE b (n INTEGER)", "");
        let bad = Raw("0003", "CREATE TABLE c (n INTEGER); NOT SQL", "");
        let version = |db: &Db| count(db, "PRAGMA user_version");

        let fresh = memory();
        fresh.migrate_user_version(1, &[&a, &b]).unwrap();
        assert_eq!(version(&fresh), 2, "a new database runs everything");
        fresh.migrate_user_version(1, &[&a, &b]).unwrap();
        assert_eq!(version(&fresh), 2, "and only once");

        let old = memory();
        old.with(|sql| sql.execute_batch("CREATE TABLE a (n INTEGER)"))
            .unwrap();
        old.migrate_user_version(1, &[&a, &b]).unwrap();
        assert_eq!(version(&old), 2, "baseline skips what the file has");

        old.migrate_user_version(1, &[&a, &b, &bad]).unwrap_err();
        assert_eq!(version(&old), 2, "a failing migration is undone");
        assert_eq!(
            count(&old, "SELECT count(*) FROM sqlite_master WHERE name = 'c'"),
            0
        );

        old.migrate_user_version(1, &[&a]).unwrap_err();
    }

    #[test]
    fn migrations_run_once() {
        each_engine(|db| {
            let posts = Raw(
                "0001",
                "CREATE TABLE posts (title TEXT)",
                "DROP TABLE posts",
            );
            db.migrate(&[&posts]).unwrap();
            db.migrate(&[&posts]).unwrap();
            assert_eq!(count(db, "SELECT count(*) FROM migrations"), 1);
        });
    }

    #[test]
    fn failed_migration_is_rolled_back_and_not_recorded() {
        each_engine(|db| {
            let broken = Raw("0001", "CREATE TABLE posts (title TEXT); NOT SQL", "");
            assert!(db.migrate(&[&broken]).is_err());
            assert_eq!(tables(db), 0);
            assert_eq!(count(db, "SELECT count(*) FROM migrations"), 0);
        });
    }

    #[test]
    fn rollback_undoes_the_last_batch_newest_first() {
        each_engine(rollback_undoes_the_last_batch);
    }

    fn rollback_undoes_the_last_batch(db: &Db) {
        let posts = Raw(
            "0001",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY)",
            "DROP TABLE posts",
        );
        let tags = Raw(
            "0002",
            "CREATE TABLE tags (post_id INTEGER REFERENCES posts (id))",
            "DROP TABLE tags",
        );
        let views = Raw("0003", "CREATE TABLE views (n INTEGER)", "DROP TABLE views");
        db.migrate(&[&posts]).unwrap();
        db.migrate(&[&posts, &tags, &views]).unwrap();
        let all: [&dyn Migration; 3] = [&posts, &tags, &views];
        assert_eq!(db.rollback(&all).unwrap(), ["0003", "0002"]);
        assert_eq!(tables(db), 1);
        assert_eq!(db.rollback(&all).unwrap(), ["0001"]);
        assert!(db.rollback(&all).unwrap().is_empty());
        db.migrate(&all).unwrap();
        assert_eq!(count(db, "SELECT count(DISTINCT batch) FROM migrations"), 1);
    }

    #[test]
    fn rollback_needs_every_migration_in_the_batch() {
        each_engine(|db| {
            db.migrate(&[&Raw(
                "0001",
                "CREATE TABLE posts (id INTEGER)",
                "DROP TABLE posts",
            )])
            .unwrap();
            let error = db.rollback(&[]).unwrap_err();
            assert!(error.contains("0001"), "{error}");
        });
    }

    struct Posts(&'static str);

    impl Seeder for Posts {
        fn run(&self, db: &Db) -> Result<(), SeedError> {
            db.table("posts").insert_only(&["title"], &[&"one"])?;
            db.with(|connection| connection.execute_batch(self.0))?;
            Ok(())
        }
    }

    #[test]
    fn seeding_commits_or_leaves_nothing() {
        each_engine(seeding_commits_or_leaves_nothing_on);
    }

    fn seeding_commits_or_leaves_nothing_on(db: &Db) {
        let posts = Raw(
            "0001",
            "CREATE TABLE posts (title TEXT)",
            "DROP TABLE posts",
        );
        db.migrate(&[&posts]).unwrap();
        db.seed(&[&Posts("SELECT 1")]).unwrap();
        assert!(db.seed(&[&Posts("NOT SQL")]).is_err());
        assert_eq!(count(db, "SELECT count(*) FROM posts"), 1);
        assert_eq!(command(db, "db:seed", &[&posts], &[&Posts("SELECT 1")]), 0);
        assert_eq!(count(db, "SELECT count(*) FROM posts"), 2);
        assert_eq!(command(db, "nope", &[], &[]), 2);
        let pending = Raw("0002", "SELECT 1", "");
        assert_eq!(
            db.status(&[&posts, &pending]).unwrap(),
            [("0001", Some(1)), ("0002", None)]
        );
        assert_eq!(memory().status(&[&posts]).unwrap(), [("0001", None)]);
    }

    /// Inserts one row, then a transaction of its own that fails.
    struct Nested;

    impl Seeder for Nested {
        fn run(&self, db: &Db) -> Result<(), SeedError> {
            db.table("posts").insert_only(&["title"], &[&"kept"])?;
            let failed: Result<()> = db.transaction(|tx| {
                tx.table("posts").insert_only(&["title"], &[&"undone"])?;
                tx.execute("NOT SQL", &[])?;
                Ok(())
            });
            assert!(failed.is_err());
            db.transaction_immediate(|tx| tx.table("posts").insert_only(&["title"], &[&"also"]))?;
            Ok(())
        }
    }

    #[test]
    fn a_transaction_inside_a_seeder_is_a_savepoint() {
        each_engine(|db| {
            db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
                .unwrap();
            db.seed(&[&Nested]).unwrap();
            assert_eq!(count(db, "SELECT count(*) FROM posts"), 2);
            assert_eq!(
                count(db, "SELECT count(*) FROM posts WHERE title = 'undone'"),
                0
            );
            // Each seeder has its own transaction: the first one is kept, the
            // second one's insert is undone.
            assert!(db.seed(&[&Nested, &Posts("NOT SQL")]).is_err());
            assert_eq!(count(db, "SELECT count(*) FROM posts"), 4);
        });
    }

    /// Inserts through a plain `Db` call, then fails.
    struct InsertThenFail;

    impl Seeder for InsertThenFail {
        fn run(&self, db: &Db) -> Result<(), SeedError> {
            db.with(|c| c.execute("INSERT INTO posts (title) VALUES ('nested')", &[]))?;
            Err("no".into())
        }
    }

    /// Hits an error and carries on, as an `is_unique_violation` check would.
    #[cfg(feature = "postgres")]
    struct SwallowsAnError;

    #[cfg(feature = "postgres")]
    impl Seeder for SwallowsAnError {
        fn run(&self, db: &Db) -> Result<(), SeedError> {
            db.table("posts").insert_only(&["title"], &[&"x"])?;
            let _ = db.with(|c| c.execute("NOT SQL", &[]));
            Ok(())
        }
    }

    #[test]
    fn db_calls_inside_a_failing_seeder_are_rolled_back_with_it() {
        each_engine(|db| {
            db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
                .unwrap();
            assert!(db.seed(&[&InsertThenFail]).is_err());
            assert_eq!(count(db, "SELECT count(*) FROM posts"), 0);
        });
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_calls_nest_by_transaction_state_not_by_sharing() {
        let Some(db) = super::testing::postgres() else {
            return;
        };
        let posts = Raw("0001", "CREATE TABLE posts (title TEXT)", "");
        db.migrate(&[&posts]).unwrap();
        // A transaction inside a plain `with` starts one (no savepoint there).
        db.with(|_| db.transaction(|tx| tx.table("posts").insert_only(&["title"], &[&"a"])))
            .unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 1);
        // `migrate` inside a transaction joins it as a savepoint: the caller's
        // rollback undoes the migration and its record.
        let tags = Raw("0002", "CREATE TABLE tags (n INTEGER)", "");
        let undone: Result<()> = db.transaction(|tx| {
            tx.table("posts").insert_only(&["title"], &[&"b"])?;
            db.migrate(&[&posts, &tags])?;
            assert_eq!(count(&db, "SELECT count(*) FROM migrations"), 2);
            Err(Error::new("undo"))
        });
        assert!(undone.is_err());
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM migrations"), 1);
        assert_eq!(tables(&db), 1);
        // And inside one that is kept, it is kept.
        db.transaction(|_| db.migrate(&[&posts, &tags])).unwrap();
        assert_eq!(tables(&db), 2);
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_refuses_to_commit_after_a_swallowed_error() {
        let Some(db) = super::testing::postgres() else {
            return;
        };
        db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
            .unwrap();
        let swallowed: Result<()> = db.transaction(|tx| {
            tx.table("posts").insert_only(&["title"], &[&"a"])?;
            let _ = tx.execute("NOT SQL", &[]);
            Ok(())
        });
        let error = swallowed.unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert_eq!(error.sqlstate(), Some("25P02"));
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 0);
        // The same inside a savepoint: only the inner one is undone.
        let outer: Result<()> = db.transaction(|tx| {
            tx.table("posts").insert_only(&["title"], &[&"kept"])?;
            let inner: Result<()> = db.transaction(|inner| {
                inner.table("posts").insert_only(&["title"], &[&"b"])?;
                let _ = inner.execute("NOT SQL", &[]);
                Ok(())
            });
            assert!(inner.is_err());
            Ok(())
        });
        outer.unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 1);
        assert!(db.seed(&[&SwallowsAnError]).is_err());
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 1);
    }

    #[test]
    fn status_of_a_fresh_database_lists_everything_pending() {
        each_engine(|db| {
            let posts = Raw("0001", "CREATE TABLE posts (title TEXT)", "");
            assert_eq!(db.status(&[&posts]).unwrap(), [("0001", None)]);
        });
    }

    #[test]
    fn clones_share_the_database() {
        each_engine(|db| {
            db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
                .unwrap();
            let other = db.clone();
            other
                .with(|sql| sql.execute("INSERT INTO posts VALUES ('a')", &[]))
                .unwrap();
            assert_eq!(count(db, "SELECT count(*) FROM posts"), 1);
        });
    }

    #[test]
    fn file_database_and_its_folder_are_created() {
        let folder = std::env::temp_dir().join(format!("rustclamp-db-{}", std::process::id()));
        let path = folder.join("nested/database.sqlite");
        Db::open(&Config::parse(&format!("DB_DATABASE={}", path.display())));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(folder);
    }

    // #34: with WAL set before busy_timeout, racing first opens failed at once
    // with "database is locked". Each Db here is its own connection, so the
    // threads contend for the file lock the way separate processes do.
    #[test]
    fn concurrent_first_opens_wait_instead_of_failing() {
        let folder = std::env::temp_dir().join(format!("rustclamp-db-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let config = format!("DB_DATABASE={}", folder.join("race.sqlite").display());
        let start = std::sync::Barrier::new(12);
        std::thread::scope(|threads| {
            for _ in 0..12 {
                threads.spawn(|| {
                    start.wait();
                    Db::open(&Config::parse(&config));
                });
            }
        });
        let _ = std::fs::remove_dir_all(folder);
    }

    // #36: the query builder works inside a transaction, through the handle.
    #[test]
    fn transaction_queries_through_the_handle() {
        each_engine(|db| {
            db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
                .unwrap();
            let failed: Result<()> = db.transaction(|tx| {
                tx.table("posts").insert_only(&["title"], &[&"a"])?;
                assert_eq!(tx.table("posts").count()?, 1);
                tx.execute("NOT SQL", &[])?;
                Ok(())
            });
            assert!(failed.is_err());
            db.transaction(|tx| tx.table("posts").insert_only(&["title"], &[&"b"]))
                .unwrap();
            assert_eq!(db.table("posts").count().unwrap(), 1);
        });
    }

    #[test]
    fn a_panic_in_a_transaction_leaves_none_open() {
        each_engine(|db| {
            db.migrate(&[&Raw("0001", "CREATE TABLE posts (title TEXT)", "")])
                .unwrap();
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _: Result<()> = db.transaction_immediate(|tx| {
                    tx.table("posts").insert_only(&["title"], &[&"a"])?;
                    panic!("boom");
                });
            }));
            assert!(panicked.is_err());
            // Still inside the transaction, the insert would show on SQLite's
            // one connection.
            assert_eq!(db.table("posts").count().unwrap(), 0);
            // A later transaction still works (and on Postgres, takes the lock).
            db.transaction_immediate(|tx| tx.table("posts").insert_only(&["title"], &[&"b"]))
                .unwrap();
        });
    }

    // Two processes that read, then write: deferred ones can both hold a read
    // lock and one gets BUSY on the upgrade; immediate ones queue instead.
    // Kept small: the busy handler retries, it does not queue fairly, so heavy
    // contention can starve one writer past its 5 s timeout on a slow CI disk.
    #[test]
    fn immediate_transactions_read_then_write_without_busy() {
        let folder =
            std::env::temp_dir().join(format!("rustclamp-db-immediate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let config = Config::parse(&format!(
            "DB_DATABASE={}",
            folder.join("tx.sqlite").display()
        ));
        Db::open(&config)
            .migrate(&[&Raw("0001", "CREATE TABLE hits (n INTEGER)", "")])
            .unwrap();
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| {
                    let db = Db::open(&config);
                    start.wait();
                    for _ in 0..10 {
                        db.transaction_immediate(|tx| {
                            let n = tx.table("hits").count()?;
                            tx.table("hits").insert_only(&["n"], &[&n])
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(Db::open(&config).table("hits").count().unwrap(), 40);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn settings_read_the_postgres_keys() {
        let settings = |text: &str| Settings::from_config(&Config::parse(text));
        let sqlite = settings("");
        assert_eq!(sqlite.database, "storage/database.sqlite");
        assert_eq!((sqlite.url, sqlite.pool), (None, 10));
        let parts = settings(
            "DB_CONNECTION=pgsql\nDB_HOST=db.internal\nDB_PORT=6543\nDB_DATABASE=shop\nDB_USERNAME=al@ice\nDB_PASSWORD=p:ss/w\nDB_SSLMODE=require\nDB_SSLROOTCERT=/etc/ca.pem\nDB_POOL=4",
        );
        assert_eq!(
            parts.url.as_deref(),
            Some(
                "postgres://al%40ice:p%3Ass%2Fw@db.internal:6543/shop?sslmode=require&sslrootcert=%2Fetc%2Fca.pem"
            )
        );
        assert_eq!(parts.pool, 4);
        assert!(!format!("{parts:?}").contains("p%3Ass"));
        let defaults = settings("DB_CONNECTION=pgsql\nDB_DATABASE=shop\nDB_USERNAME=app");
        assert_eq!(
            defaults.url.as_deref(),
            Some("postgres://app:@127.0.0.1:5432/shop")
        );
        let url = settings(
            "DB_CONNECTION=pgsql\nDATABASE_URL=postgres://u@h/d\nDB_HOST=ignored\nDB_POOL=nonsense",
        );
        assert_eq!(url.url.as_deref(), Some("postgres://u@h/d"));
        assert_eq!(url.pool, 10);
    }

    #[cfg(not(feature = "postgres"))]
    #[test]
    fn pgsql_needs_the_postgres_feature() {
        let settings = Settings::from_config(&Config::parse("DB_CONNECTION=pgsql\nDB_DATABASE=x"));
        let stopped = std::panic::catch_unwind(|| Db::connect(&settings)).unwrap_err();
        let message = stopped.downcast_ref::<String>().unwrap();
        assert!(message.contains("`postgres` feature"), "{message}");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn an_unreachable_postgres_stops_startup() {
        let settings = |url: &str| Settings {
            connection: "pgsql".into(),
            database: String::new(),
            url: Some(url.into()),
            pool: 2,
        };
        // Port 1: nothing listens there.
        let error = Db::try_connect(&settings(
            "postgres://u:secret@127.0.0.1:1/d?sslmode=disable",
        ))
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.starts_with("cannot open database 127.0.0.1:1/d"),
            "{message}"
        );
        assert!(!message.contains("secret"), "{message}");
        let error = Db::try_connect(&settings("mysql://nope")).unwrap_err();
        assert!(error.to_string().contains("postgres://"), "{error}");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_only_behaviour() {
        let Some(db) = super::testing::postgres() else {
            return;
        };
        let posts = Raw("0001", "CREATE TABLE posts (id INTEGER, n INTEGER)", "");
        let error = db.migrate_user_version(0, &[&posts]).unwrap_err();
        assert!(error.to_string().contains("SQLite only"), "{error}");

        // Instances that migrate at startup take turns on the advisory lock.
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| db.migrate(&[&posts]).unwrap());
            }
        });
        assert_eq!(count(&db, "SELECT count(*) FROM migrations"), 1);

        // A read-then-write that would hand out duplicates without the lock.
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| {
                    for _ in 0..10 {
                        db.transaction_immediate(|tx| {
                            let n = tx.table("posts").count()?;
                            tx.table("posts").insert_only(&["n"], &[&n])
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(count(&db, "SELECT count(DISTINCT n) FROM posts"), 40);

        // A plain transaction does not take that lock.
        db.transaction(|tx| tx.table("posts").insert_only(&["n"], &[&99]))
            .unwrap();
        // A failure inside a transaction rolls it back and frees its client.
        let failed: Result<()> = db.transaction(|tx| {
            tx.table("posts").insert_only(&["n"], &[&100])?;
            tx.execute("NOT SQL", &[])?;
            Ok(())
        });
        assert!(failed.unwrap_err().sqlstate() == Some("42601"));
        assert_eq!(count(&db, "SELECT count(*) FROM posts WHERE n = 100"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM posts WHERE n = 99"), 1);
    }

    #[test]
    fn reads_on_a_file_database_do_not_wait_for_the_writer() {
        let folder =
            std::env::temp_dir().join(format!("rustclamp-db-readers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let db = Db::open(&Config::parse(&format!(
            "DB_DATABASE={}",
            folder.join("read.sqlite").display()
        )));
        db.with(|sql| sql.execute_batch("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (1)"))
            .unwrap();
        // Holding the writer: a read through it would wait forever.
        db.with(|sql| {
            sql.execute_batch("BEGIN; INSERT INTO t VALUES (2)")
                .unwrap();
            assert_eq!(
                db.table("t").count().unwrap(),
                1,
                "the open transaction is not seen"
            );
            sql.execute_batch("COMMIT").unwrap();
        });
        assert_eq!(db.table("t").count().unwrap(), 2, "committed rows are seen");
        let write = db.read(|sql| sql.execute("INSERT INTO t VALUES (3)", &[]));
        assert!(write.is_err(), "readers are read-only");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn try_connect_reports_instead_of_panicking() {
        let folder = std::env::temp_dir().join(format!("rustclamp-db-try-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let settings = |database: &std::path::Path, connection: &str| Settings {
            connection: connection.into(),
            database: database.display().to_string(),
            url: None,
            pool: 10,
        };
        let missing = folder.join("missing/database.sqlite");
        assert!(matches!(
            Db::try_connect(&settings(&missing, "sqlite")),
            Err(DbError::Open { .. })
        ));
        assert!(!folder.exists(), "no folder is created");
        assert!(matches!(
            Db::try_connect(&settings(&missing, "mysql")),
            Err(DbError::Engine(engine)) if engine == "mysql"
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let junk = folder.join("junk.sqlite");
        let bytes = b"not a database, but somebody's file".repeat(40);
        std::fs::write(&junk, &bytes).unwrap();
        let error = Db::try_connect(&settings(&junk, "sqlite")).unwrap_err();
        assert!(
            error.to_string().starts_with("cannot open database"),
            "{error}"
        );
        assert_eq!(std::fs::read(&junk).unwrap(), bytes, "left untouched");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn keep_leaves_the_journal_mode_and_header_alone() {
        let folder = std::env::temp_dir().join(format!("rustclamp-db-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("rollback.sqlite");
        {
            let plain = rusqlite::Connection::open(&path).unwrap();
            plain.execute_batch("CREATE TABLE t (n INTEGER)").unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let settings = Settings {
            connection: "sqlite".into(),
            database: path.display().to_string(),
            url: None,
            pool: 10,
        };
        let mode = |db: &Db| {
            db.with(|sql| sql.query_row("PRAGMA journal_mode", &[], |row| row.get::<String>(0)))
                .unwrap()
        };
        let kept = Db::try_connect_with(&settings, Journal::Keep).unwrap();
        assert_eq!(mode(&kept), "delete");
        drop(kept);
        assert_eq!(std::fs::read(&path).unwrap(), before, "header rewritten");
        assert_eq!(mode(&Db::try_connect(&settings).unwrap()), "wal");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn unsupported_engine_stops_the_app_without_a_folder() {
        let folder =
            std::env::temp_dir().join(format!("rustclamp-db-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let settings = Settings {
            connection: "mysql".into(),
            database: folder.join("database.sqlite").display().to_string(),
            url: None,
            pool: 10,
        };
        let stopped = std::panic::catch_unwind(|| Db::connect(&settings)).unwrap_err();
        let message = stopped.downcast_ref::<String>().unwrap();
        assert!(message.contains("DB_CONNECTION"), "{message}");
        assert!(!folder.exists(), "no folder for an engine that is refused");
    }
}
