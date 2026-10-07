//! A SQLite database, configured from `.env`.
//!
//! Enabled by the `db` feature, which `clamp init --web` turns on. SQLite is
//! compiled into the app, so there is no server to run. `DB_CONNECTION`
//! names the engine (only `sqlite` so far) and `DB_DATABASE` the file,
//! `storage/database.sqlite` by default, or `:memory:`.
//!
//! Use the re-exported [`sqlite`] (rusqlite) rather than adding `rusqlite` to
//! the app: two versions of it cannot link into one binary.
//!
//! A migration is a struct implementing [`Migration`], one per file, with
//! `up` and `down` SQL written by [`Schema`] or by hand. [`Db::table`] builds
//! queries; [`Db::with`] lends the connection for anything else.
//!
//! ```
//! use rustclamp::config::Config;
//! use rustclamp::db::{Db, Migration, Schema, sqlite::params};
//!
//! struct CreatePosts;
//!
//! impl Migration for CreatePosts {
//!     fn name(&self) -> &'static str {
//!         "2026_09_29_000001_create_posts"
//!     }
//!     fn up(&self) -> String {
//!         Schema::create("posts", |table| {
//!             table.id();
//!             table.string("title");
//!             table.timestamps();
//!         })
//!     }
//!     fn down(&self) -> String {
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
//!     .get(|row| row.get::<_, String>("title"))
//!     .unwrap();
//! assert_eq!(titles, ["Hello"]);
//! ```

use std::sync::{Arc, Mutex, PoisonError};

use crate::config::Config;

mod query;
mod schema;
mod states;
pub mod timestamp;
pub use query::{Page, Query};
pub use schema::{Column, OnDelete, Schema, Table};
pub use states::{Change, States, Transition};

pub use rusqlite as sqlite;
use rusqlite::Connection;
/// `#[derive(Model)]` with `#[model(table = "posts")]`: reads each field
/// from the column of the same name. See [`Model`](trait@Model).
pub use rustclamp_macros::Model;

/// A shared database. Clones share the same connections: one writer behind
/// a lock, and for a WAL file database, read-only connections that
/// [`Db::read`] (and a [`Query`]'s `get`, `first` and `count`) take, so reads
/// do not wait for the writer or for each other (#39).
#[derive(Clone)]
pub struct Db {
    // `:memory:` relies on the single writer: every clone must see the same
    // in-memory database, so it gets no readers.
    connection: Arc<Mutex<Connection>>,
    readers: Option<Arc<Readers>>,
}

/// Idle read-only connections to the same file, opened on demand.
struct Readers {
    path: String,
    // ponytail: grows to the peak number of concurrent readers and keeps them
    // all; cap it if an app with thousands of threads shows up.
    idle: Mutex<Vec<Connection>>,
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
fn open_reader(path: &str) -> sqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        sqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | sqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}

/// Why [`Db::try_connect`] could not open the database.
#[derive(Debug)]
pub enum DbError {
    /// `DB_CONNECTION` names an engine other than `sqlite`.
    Engine(String),
    /// SQLite could not open or set up the file.
    Open {
        /// The `DB_DATABASE` path.
        path: String,
        /// What SQLite said.
        source: sqlite::Error,
    },
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(engine) => write!(
                f,
                "config key DB_CONNECTION is {engine}; only sqlite is supported"
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
#[derive(Debug, Clone)]
pub struct Settings {
    /// The engine; only `sqlite` so far.
    pub connection: String,
    /// The SQLite file, or `:memory:`.
    pub database: String,
}

impl Settings {
    /// `DB_CONNECTION` (`sqlite`) and `DB_DATABASE`
    /// (`storage/database.sqlite`).
    pub fn from_config(config: &Config) -> Self {
        // ponytail: the same defaults as the web template's
        // `app/config/database.rs`, for apps without one
        Self {
            connection: config.get("DB_CONNECTION").unwrap_or("sqlite").into(),
            database: config
                .get("DB_DATABASE")
                .unwrap_or("storage/database.sqlite")
                .into(),
        }
    }
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
    /// When the connection is not `sqlite` or the database cannot be opened:
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
    /// [`DbError::Engine`] when the connection is not `sqlite`,
    /// [`DbError::Open`] when the database cannot be opened.
    pub fn try_connect(settings: &Settings) -> Result<Self, DbError> {
        Self::try_connect_with(settings, Journal::Wal)
    }

    /// [`Db::try_connect`], choosing the journal mode: [`Journal::Keep`]
    /// leaves the file's own mode alone, so a file that uses the rollback
    /// journal is opened without rewriting its header.
    ///
    /// # Errors
    ///
    /// As [`Db::try_connect`].
    pub fn try_connect_with(settings: &Settings, journal: Journal) -> Result<Self, DbError> {
        let engine = settings.connection.as_str();
        if engine != "sqlite" {
            return Err(DbError::Engine(engine.to_owned()));
        }
        let path = settings.database.as_str();
        let connection = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(path).and_then(|connection| {
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
            source,
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
            connection: Arc::new(Mutex::new(connection)),
            readers,
        })
    }

    /// Runs `work` with the connection, holding it for the duration.
    pub fn with<T>(&self, work: impl FnOnce(&Connection) -> T) -> T {
        // A panic in another request leaves the connection usable; SQLite
        // rolls back its unfinished transaction.
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        work(&connection)
    }

    /// Runs `work` with a read-only connection, sharing the database with
    /// other readers and with the writer ([`Db::with`]) instead of waiting
    /// for them. It sees everything committed before it started, but not a
    /// transaction still open on the writer; a write through it fails. An
    /// in-memory database, or one opened with [`Journal::Keep`], has one
    /// connection, and this is [`Db::with`].
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::Db;
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// let one: i64 = db.read(|sql| sql.query_row("SELECT 1", [], |row| row.get(0))).unwrap();
    /// assert_eq!(one, 1);
    /// ```
    pub fn read<T>(&self, work: impl FnOnce(&Connection) -> T) -> T {
        let Some(readers) = &self.readers else {
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
        let value = work(&connection);
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
    /// let one: i64 = block_on(db.blocking(|sql| sql.query_row("SELECT 1", [], |row| row.get(0)))).unwrap();
    /// assert_eq!(one, 1);
    /// ```
    pub fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection) -> T + Send + 'static,
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
    /// The write lock is taken at the first write, so two transactions that
    /// both read and then write can meet a "database is locked" error; use
    /// [`Db::transaction_immediate`] for those.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, sqlite};
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch("CREATE TABLE t (n INTEGER)")).unwrap();
    /// let failed: Result<(), sqlite::Error> = db.transaction(|tx| {
    ///     tx.table("t").insert(&["n"], [&1])?;
    ///     tx.execute("NOT SQL", [])?;
    ///     Ok(())
    /// });
    /// assert!(failed.is_err());
    /// assert_eq!(db.table("t").count().unwrap(), 0, "the insert was undone");
    /// ```
    pub fn transaction<T, E: From<sqlite::Error>>(
        &self,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.run_transaction(sqlite::TransactionBehavior::Deferred, work)
    }

    /// [`Db::transaction`], but it takes the write lock up front (`BEGIN
    /// IMMEDIATE`), waiting out the busy timeout, so a read-then-write
    /// cannot fail halfway on a lock another process took meanwhile. Inside
    /// another transaction it is a savepoint of that one, which keeps
    /// whatever lock that one has.
    pub fn transaction_immediate<T, E: From<sqlite::Error>>(
        &self,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.run_transaction(sqlite::TransactionBehavior::Immediate, work)
    }

    fn run_transaction<T, E: From<sqlite::Error>>(
        &self,
        behavior: sqlite::TransactionBehavior,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // rusqlite's Transaction and Savepoint roll back when dropped, so an
        // `Err` or a panic in `work` leaves no transaction open behind it.
        if connection.is_autocommit() {
            let transaction = connection.transaction_with_behavior(behavior)?;
            let value = work(&Tx(&transaction))?;
            transaction.commit()?;
            Ok(value)
        } else {
            let savepoint = connection.savepoint()?;
            let value = work(&Tx(&savepoint))?;
            savepoint.commit()?;
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
    pub fn migrate(&self, migrations: &[&dyn Migration]) -> sqlite::Result<()> {
        self.with(|connection| {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS migrations (
                    name TEXT PRIMARY KEY,
                    batch INTEGER NOT NULL,
                    ran_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                )",
            )?;
            let batch: i64 = connection.query_row(
                "SELECT coalesce(max(batch), 0) + 1 FROM migrations",
                [],
                |row| row.get(0),
            )?;
            for migration in migrations {
                let name = migration.name();
                let ran: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM migrations WHERE name = ?1)",
                    [name],
                    |row| row.get(0),
                )?;
                if ran {
                    continue;
                }
                savepoint(connection, |connection| {
                    connection.execute_batch(&migration.up())?;
                    connection.execute(
                        "INSERT INTO migrations (name, batch) VALUES (?1, ?2)",
                        rusqlite::params![name, batch],
                    )
                })?;
            }
            Ok(())
        })
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
    /// When SQL fails, or the database is at a version beyond `migrations`
    /// (it was migrated by a newer build).
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, Migration};
    ///
    /// struct Sql(&'static str, &'static str);
    /// impl Migration for Sql {
    ///     fn name(&self) -> &'static str { self.0 }
    ///     fn up(&self) -> String { self.1.into() }
    ///     fn down(&self) -> String { String::new() }
    /// }
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// // An existing database that already has the first table.
    /// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY)")).unwrap();
    /// let first = Sql("0001", "CREATE TABLE posts (id INTEGER PRIMARY KEY);");
    /// let second = Sql("0002", "CREATE TABLE tags (id INTEGER PRIMARY KEY);");
    /// db.migrate_user_version(1, &[&first, &second]).unwrap();
    /// assert_eq!(db.with(|sql| sql.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))).unwrap(), 2);
    /// ```
    pub fn migrate_user_version(
        &self,
        baseline: usize,
        migrations: &[&dyn Migration],
    ) -> sqlite::Result<()> {
        self.with(|connection| {
            let version = |connection: &Connection| -> sqlite::Result<i64> {
                connection.pragma_query_value(None, "user_version", |row| row.get(0))
            };
            let mut current = version(connection)?;
            if current == 0 && baseline > 0 {
                let has_tables: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
                    [],
                    |row| row.get(0),
                )?;
                if has_tables {
                    // `baseline` is a count of migrations, never near i64::MAX.
                    current = baseline as i64;
                    connection.pragma_update(None, "user_version", current)?;
                }
            }
            if current < 0 || current as usize > migrations.len() {
                return Err(sqlite::Error::InvalidParameterName(format!(
                    "database user_version {current} is beyond the {} known migrations",
                    migrations.len()
                )));
            }
            for (index, migration) in migrations.iter().enumerate().skip(current as usize) {
                savepoint(connection, |connection| {
                    connection.execute_batch(&migration.up())?;
                    connection.pragma_update(None, "user_version", index as i64 + 1)
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
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'migrations')",
                    [],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Ok(Vec::new());
                }
                connection
                    .prepare("SELECT name, batch FROM migrations")?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|error: sqlite::Error| error.to_string())?;
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
        for seeder in seeders {
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
            let names: Vec<String> = connection
                .prepare(
                    "SELECT name FROM migrations
                     WHERE batch = (SELECT max(batch) FROM migrations)
                     ORDER BY rowid DESC",
                )
                .and_then(|mut query| query.query_map([], |row| row.get(0))?.collect())
                .map_err(|error| error.to_string())?;
            let mut undone = Vec::new();
            for name in names {
                let migration = migrations
                    .iter()
                    .find(|migration| migration.name() == name)
                    .ok_or_else(|| format!("migration {name} ran but is not in the list"))?;
                savepoint(connection, |connection| {
                    connection.execute_batch(&migration.down())?;
                    connection.execute("DELETE FROM migrations WHERE name = ?1", [&name])
                })
                .map_err(|error: sqlite::Error| format!("{name}: {error}"))?;
                undone.push(migration.name());
            }
            Ok(undone)
        })
    }
}

/// The connection inside [`Db::transaction`]. It derefs to [`Connection`]
/// for raw SQL, and [`Tx::table`] builds queries that run in the transaction.
pub struct Tx<'a>(&'a Connection);

impl<'a> Tx<'a> {
    /// A query on `table` inside this transaction; see [`Query`].
    pub fn table(&self, table: &str) -> Query<'a> {
        Query::in_transaction(self.0, table)
    }
}

impl std::ops::Deref for Tx<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.0
    }
}

/// A table's rows as a Rust type, like a Laravel model: name the table and
/// say how a row becomes `Self` once, then query with the builder.
/// `#[derive(Model)]` does both, reading each field from the column of the
/// same name.
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, Model, sqlite::params};
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
/// # use rustclamp::db::{Db, Model, sqlite::ToSql};
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
/// let ids: Vec<&dyn ToSql> = all.iter().map(|c| &c.post_id as &dyn ToSql).collect();
/// let posts = Post::query(&db).where_in("id", &ids).get(Post::from_row).unwrap();
/// assert_eq!(posts.len(), 2);
/// ```
///
/// By hand, when a field is not a column of the same name:
///
/// ```
/// use rustclamp::db::{Model, sqlite::{Result, Row}};
///
/// struct Tag {
///     label: String,
/// }
///
/// impl Model for Tag {
///     const TABLE: &'static str = "tags";
///
///     fn from_row(row: &Row<'_>) -> Result<Self> {
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
    fn from_row(row: &sqlite::Row<'_>) -> sqlite::Result<Self>;

    /// A query on [`TABLE`](Self::TABLE); finish it with
    /// `.get(Self::from_row)` or `.first(Self::from_row)`.
    fn query(db: &Db) -> Query<'_> {
        db.table(Self::TABLE)
    }

    /// Every row.
    fn all(db: &Db) -> sqlite::Result<Vec<Self>> {
        Self::query(db).get(Self::from_row)
    }

    /// The row whose `id` is `id`.
    fn find(db: &Db, id: i64) -> sqlite::Result<Option<Self>> {
        Self::query(db).where_eq("id", &id).first(Self::from_row)
    }

    /// The row whose `public_id` column is `public_id`, the UUID that goes in
    /// URLs so `id` never leaves the app. `None` for text that is not a UUID.
    fn find_public(db: &Db, public_id: &str) -> sqlite::Result<Option<Self>> {
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
    fn up(&self) -> String {
        self.1.up()
    }
    fn down(&self) -> String {
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
    pub fn model<M: Model>(&self, param: &str) -> sqlite::Result<Option<M>> {
        match self.param(param) {
            Some(public_id) => M::find_public(self.db(), public_id),
            None => Ok(None),
        }
    }
}

/// Stored as its hyphenated text, so it reads well in the database and in
/// `public_id` columns.
impl sqlite::types::ToSql for crate::uuid::Uuid {
    fn to_sql(&self) -> sqlite::Result<sqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl sqlite::types::FromSql for crate::uuid::Uuid {
    fn column_result(value: sqlite::types::ValueRef<'_>) -> sqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse(text)
            .ok_or_else(|| sqlite::types::FromSqlError::Other(format!("not a UUID: {text}").into()))
    }
}

/// Runs `work` inside a savepoint: a transaction of its own, or a nested one
/// inside a transaction already open.
fn savepoint<T, E: From<sqlite::Error>>(
    connection: &Connection,
    work: impl FnOnce(&Connection) -> Result<T, E>,
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
    fn up(&self) -> String;
    /// The SQL that undoes [`up`](Self::up), often [`Schema::drop`].
    fn down(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "web")]
    #[test]
    fn model_binds_by_public_id() {
        use crate::web::{Request, Router, error};
        struct Post {
            title: String,
        }
        impl Model for Post {
            const TABLE: &'static str = "posts";
            fn from_row(row: &sqlite::Row<'_>) -> sqlite::Result<Self> {
                Ok(Self {
                    title: row.get("title")?,
                })
            }
        }
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let public_id = crate::uuid::Uuid::v7();
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, public_id TEXT, title TEXT)",
            )?;
            sql.execute("INSERT INTO posts VALUES (7, ?1, 'Hi')", [&public_id])
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

    /// A migration from literal SQL.
    struct Sql(&'static str, &'static str, &'static str);

    impl Migration for Sql {
        fn name(&self) -> &'static str {
            self.0
        }
        fn up(&self) -> String {
            self.1.to_owned()
        }
        fn down(&self) -> String {
            self.2.to_owned()
        }
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|connection| connection.query_row(sql, [], |row| row.get(0)))
            .unwrap()
    }

    fn memory() -> Db {
        Db::open(&Config::parse("DB_DATABASE=:memory:"))
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
    fn blocking_runs_off_the_calling_thread_and_shares_the_connection() {
        let db = memory();
        let caller = std::thread::current().id();
        let (ran_on, one) = block_on(db.blocking(|sql| {
            sql.execute_batch("CREATE TABLE t (n INTEGER); INSERT INTO t VALUES (1)")
                .unwrap();
            (std::thread::current().id(), 1)
        }));
        assert_ne!(ran_on, caller);
        assert_eq!(one, 1);
        assert_eq!(count(&db, "SELECT count(*) FROM t"), 1, "same connection");
        // Many at once all complete.
        let all: Vec<_> = (0..8)
            .map(|n| db.blocking(move |sql| sql.execute("INSERT INTO t VALUES (?1)", [n])))
            .collect();
        for one in all {
            assert_eq!(block_on(one).unwrap(), 1);
        }
        assert_eq!(count(&db, "SELECT count(*) FROM t"), 9);
    }

    #[test]
    fn blocking_panics_reach_the_awaiter_and_leave_the_db_usable() {
        let db = memory();
        let caught = std::panic::catch_unwind(|| block_on(db.blocking(|_| panic!("boom"))));
        assert!(caught.is_err());
        assert_eq!(count(&db, "SELECT 1"), 1);
    }

    #[test]
    fn user_version_runs_pending_and_adopts_existing() {
        let a = Sql("0001", "CREATE TABLE a (n INTEGER)", "");
        let b = Sql("0002", "CREATE TABLE b (n INTEGER)", "");
        let bad = Sql("0003", "CREATE TABLE c (n INTEGER); NOT SQL", "");
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
        let db = memory();
        let posts = Sql(
            "0001",
            "CREATE TABLE posts (title TEXT)",
            "DROP TABLE posts",
        );
        db.migrate(&[&posts]).unwrap();
        db.migrate(&[&posts]).unwrap();
        let ran: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM migrations", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(ran, 1);
    }

    #[test]
    fn failed_migration_is_rolled_back_and_not_recorded() {
        let db = memory();
        let broken = Sql("0001", "CREATE TABLE posts (title TEXT); NOT SQL", "");
        assert!(db.migrate(&[&broken]).is_err());
        let tables: i64 = db
            .with(|sql| {
                sql.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'posts'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn rollback_undoes_the_last_batch_newest_first() {
        let db = memory();
        let posts = Sql(
            "0001",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY)",
            "DROP TABLE posts",
        );
        let tags = Sql(
            "0002",
            "CREATE TABLE tags (post_id INTEGER REFERENCES posts (id))",
            "DROP TABLE tags",
        );
        let views = Sql("0003", "CREATE TABLE views (n INTEGER)", "DROP TABLE views");
        db.migrate(&[&posts]).unwrap();
        db.migrate(&[&posts, &tags, &views]).unwrap();
        let all: [&dyn Migration; 3] = [&posts, &tags, &views];
        assert_eq!(db.rollback(&all).unwrap(), ["0003", "0002"]);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name != 'migrations'"
            ),
            1
        );
        assert_eq!(db.rollback(&all).unwrap(), ["0001"]);
        assert!(db.rollback(&all).unwrap().is_empty());
        db.migrate(&all).unwrap();
        assert_eq!(
            count(&db, "SELECT count(DISTINCT batch) FROM migrations"),
            1
        );
    }

    #[test]
    fn rollback_needs_every_migration_in_the_batch() {
        let db = memory();
        db.migrate(&[&Sql(
            "0001",
            "CREATE TABLE posts (id INTEGER)",
            "DROP TABLE posts",
        )])
        .unwrap();
        let error = db.rollback(&[]).unwrap_err();
        assert!(error.contains("0001"), "{error}");
    }

    struct Posts(&'static str);

    impl Seeder for Posts {
        fn run(&self, db: &Db) -> Result<(), SeedError> {
            db.table("posts").insert(&["title"], [&"one"])?;
            db.with(|connection| connection.execute_batch(self.0))?;
            Ok(())
        }
    }

    #[test]
    fn seeding_commits_or_leaves_nothing() {
        let db = memory();
        let posts = Sql(
            "0001",
            "CREATE TABLE posts (title TEXT)",
            "DROP TABLE posts",
        );
        db.migrate(&[&posts]).unwrap();
        db.seed(&[&Posts("SELECT 1")]).unwrap();
        assert!(db.seed(&[&Posts("NOT SQL")]).is_err());
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 1);
        assert_eq!(command(&db, "db:seed", &[&posts], &[&Posts("SELECT 1")]), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM posts"), 2);
        assert_eq!(command(&db, "nope", &[], &[]), 2);
        let pending = Sql("0002", "SELECT 1", "");
        assert_eq!(
            db.status(&[&posts, &pending]).unwrap(),
            [("0001", Some(1)), ("0002", None)]
        );
        assert_eq!(memory().status(&[&posts]).unwrap(), [("0001", None)]);
    }

    #[test]
    fn clones_share_the_database() {
        let db = memory();
        db.migrate(&[&Sql("0001", "CREATE TABLE posts (title TEXT)", "")])
            .unwrap();
        let other = db.clone();
        other
            .with(|sql| sql.execute("INSERT INTO posts VALUES ('a')", []))
            .unwrap();
        let count: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM posts", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(count, 1);
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
        let db = memory();
        db.migrate(&[&Sql("0001", "CREATE TABLE posts (title TEXT)", "")])
            .unwrap();
        let failed: sqlite::Result<()> = db.transaction(|tx| {
            tx.table("posts").insert(&["title"], [&"a"])?;
            assert_eq!(tx.table("posts").count()?, 1);
            tx.execute("NOT SQL", [])?;
            Ok(())
        });
        assert!(failed.is_err());
        db.transaction(|tx| tx.table("posts").insert(&["title"], [&"b"]))
            .unwrap();
        assert_eq!(db.table("posts").count().unwrap(), 1);
    }

    #[test]
    fn a_panic_in_a_transaction_leaves_none_open() {
        let db = memory();
        db.migrate(&[&Sql("0001", "CREATE TABLE posts (title TEXT)", "")])
            .unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: sqlite::Result<()> = db.transaction_immediate(|tx| {
                tx.table("posts").insert(&["title"], [&"a"])?;
                panic!("boom");
            });
        }));
        assert!(panicked.is_err());
        assert!(db.with(Connection::is_autocommit));
        assert_eq!(db.table("posts").count().unwrap(), 0);
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
            .migrate(&[&Sql("0001", "CREATE TABLE hits (n INTEGER)", "")])
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
                            tx.table("hits").insert(&["n"], [&n])
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
        let write = db.read(|sql| sql.execute("INSERT INTO t VALUES (3)", []));
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
            let plain = Connection::open(&path).unwrap();
            plain.execute_batch("CREATE TABLE t (n INTEGER)").unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let settings = Settings {
            connection: "sqlite".into(),
            database: path.display().to_string(),
        };
        let mode = |db: &Db| {
            db.with(|sql| {
                sql.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            })
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
        };
        let stopped = std::panic::catch_unwind(|| Db::connect(&settings)).unwrap_err();
        let message = stopped.downcast_ref::<String>().unwrap();
        assert!(message.contains("DB_CONNECTION"), "{message}");
        assert!(!folder.exists(), "no folder for an engine that is refused");
    }
}
