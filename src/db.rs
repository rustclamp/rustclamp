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
pub use query::Query;
pub use schema::{Column, Schema, Table};
pub use states::{Change, States, Transition};

pub use rusqlite as sqlite;
use rusqlite::Connection;

/// A shared database connection. Clones share the same connection.
#[derive(Clone)]
pub struct Db {
    // ponytail: one connection behind a lock serializes queries across request
    // threads; a pool when that shows up in measurements. `:memory:` relies on
    // it: every clone must see the same in-memory database.
    connection: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

impl Db {
    /// Opens the database named by `DB_CONNECTION` and `DB_DATABASE`,
    /// creating the file and its folder when missing.
    ///
    /// # Panics
    ///
    /// When `DB_CONNECTION` is not `sqlite` or the database cannot be opened:
    /// the app should stop at startup rather than run without its data.
    pub fn open(config: &Config) -> Self {
        let engine = config.get("DB_CONNECTION").unwrap_or("sqlite");
        assert!(
            engine == "sqlite",
            "config key DB_CONNECTION is {engine}; only sqlite is supported"
        );
        let path = config
            .get("DB_DATABASE")
            .unwrap_or("storage/database.sqlite");
        let connection = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            if let Some(folder) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(folder);
            }
            Connection::open(path).and_then(|connection| {
                // WAL lets readers work while a write is in progress.
                connection.pragma_update(None, "journal_mode", "WAL")?;
                connection.busy_timeout(std::time::Duration::from_secs(5))?;
                Ok(connection)
            })
        }
        .and_then(|connection| {
            connection.pragma_update(None, "foreign_keys", true)?;
            Ok(connection)
        })
        .unwrap_or_else(|error| panic!("cannot open database {path}: {error}"));
        Self {
            connection: Arc::new(Mutex::new(connection)),
        }
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
                let transaction = connection.unchecked_transaction()?;
                transaction.execute_batch(&migration.up())?;
                transaction.execute(
                    "INSERT INTO migrations (name, batch) VALUES (?1, ?2)",
                    rusqlite::params![name, batch],
                )?;
                transaction.commit()?;
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
    pub fn seed(&self, seeders: &[&dyn Seeder]) -> sqlite::Result<()> {
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
                let transaction = connection
                    .unchecked_transaction()
                    .map_err(|error| error.to_string())?;
                transaction
                    .execute_batch(&migration.down())
                    .and_then(|()| {
                        transaction.execute("DELETE FROM migrations WHERE name = ?1", [&name])
                    })
                    .map_err(|error| format!("{name}: {error}"))?;
                transaction.commit().map_err(|error| error.to_string())?;
                undone.push(migration.name());
            }
            Ok(undone)
        })
    }
}

/// A table's rows as a Rust type, like a Laravel model: name the table and
/// say how a row becomes `Self` once, then query with the builder.
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, Model, sqlite::{Result, Row, params}};
///
/// struct Post {
///     id: i64,
///     title: String,
/// }
///
/// impl Model for Post {
///     const TABLE: &'static str = "posts";
///
///     fn from_row(row: &Row<'_>) -> Result<Self> {
///         Ok(Self { id: row.get("id")?, title: row.get("title")? })
///     }
/// }
///
/// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
/// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT)"))
///     .unwrap();
/// let id = Post::query(&db).insert(&["title"], params!["Hello"]).unwrap();
/// assert_eq!(Post::find(&db, id).unwrap().unwrap().title, "Hello");
/// let titles: Vec<Post> = Post::query(&db).order_by_desc("id").get(Post::from_row).unwrap();
/// assert_eq!(titles[0].id, id);
/// assert_eq!(Post::all(&db).unwrap().len(), 1);
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
}

/// Fills the database with data, like a Laravel seeder class: one struct
/// per file in `app/database/seeders/`, run by `cargo run -- db:seed`.
pub trait Seeder {
    /// Inserts the data, usually with [`Db::table`].
    fn run(&self, db: &Db) -> sqlite::Result<()>;
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
            .and_then(|()| db.seed(seeders))
            .map_err(|error| error.to_string())
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
    /// `2026_09_29_000001_create_posts`. Usually the file's own name:
    /// `migration_name(file!())`.
    fn name(&self) -> &'static str;
    /// The SQL that applies the change, often [`Schema::create`].
    fn up(&self) -> String;
    /// The SQL that undoes [`up`](Self::up), often [`Schema::drop`].
    fn down(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fn run(&self, db: &Db) -> sqlite::Result<()> {
            db.table("posts").insert(&["title"], [&"one"])?;
            db.with(|connection| connection.execute_batch(self.0))
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

    #[test]
    #[should_panic(expected = "DB_CONNECTION")]
    fn unsupported_engine_stops_the_app() {
        Db::open(&Config::parse("DB_CONNECTION=mysql"));
    }
}
