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
//! ```
//! use rustclamp::config::Config;
//! use rustclamp::db::Db;
//!
//! let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
//! db.migrate(&[("0001_create_posts", "CREATE TABLE posts (title TEXT NOT NULL)")])
//!     .unwrap();
//! db.with(|sql| sql.execute("INSERT INTO posts (title) VALUES (?1)", ["Hello"]))
//!     .unwrap();
//! let count: i64 = db
//!     .with(|sql| sql.query_row("SELECT count(*) FROM posts", [], |row| row.get(0)))
//!     .unwrap();
//! assert_eq!(count, 1);
//! ```

use std::sync::{Arc, Mutex, PoisonError};

use crate::config::Config;

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

    /// Runs each `(name, sql)` migration not yet run, in order, recording it
    /// in the `migrations` table. A migration that fails is rolled back and
    /// stops the rest.
    pub fn migrate(&self, migrations: &[(&str, &str)]) -> sqlite::Result<()> {
        self.with(|connection| {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS migrations (
                    name TEXT PRIMARY KEY,
                    ran_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                )",
            )?;
            for (name, sql) in migrations {
                let ran: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM migrations WHERE name = ?1)",
                    [name],
                    |row| row.get(0),
                )?;
                if ran {
                    continue;
                }
                let transaction = connection.unchecked_transaction()?;
                transaction.execute_batch(sql)?;
                transaction.execute("INSERT INTO migrations (name) VALUES (?1)", [name])?;
                transaction.commit()?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Db {
        Db::open(&Config::parse("DB_DATABASE=:memory:"))
    }

    #[test]
    fn migrations_run_once() {
        let db = memory();
        let migrations = [("0001", "CREATE TABLE posts (title TEXT)")];
        db.migrate(&migrations).unwrap();
        db.migrate(&migrations).unwrap();
        let ran: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM migrations", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(ran, 1);
    }

    #[test]
    fn failed_migration_is_rolled_back_and_not_recorded() {
        let db = memory();
        let migrations = [("0001", "CREATE TABLE posts (title TEXT); NOT SQL")];
        assert!(db.migrate(&migrations).is_err());
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
    fn clones_share_the_database() {
        let db = memory();
        db.migrate(&[("0001", "CREATE TABLE posts (title TEXT)")])
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
