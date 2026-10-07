//! Model states with allowed transitions and a recorded history, like
//! spatie/laravel-model-states with an activity log.

use std::fmt;
use std::time::SystemTime;

use super::{Connection, Db, Error, Param, Result, Tx, params, query::name, savepoint, timestamp};

/// The states a column may hold and the moves allowed between them. With
/// [`States::with_history`], every move is also recorded in the
/// `state_history` table (created when missing): which row, from and to
/// what, who made it and when.
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, States, Transition};
///
/// const ORDER: States = States::new(
///     "orders",
///     "status",
///     &[("pending", "paid"), ("pending", "cancelled"), ("paid", "shipped")],
/// )
/// .with_history();
///
/// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
/// db.with(|sql| {
///     sql.execute_batch(
///         "CREATE TABLE orders (id INTEGER PRIMARY KEY, status TEXT NOT NULL DEFAULT 'pending');
///          INSERT INTO orders DEFAULT VALUES;",
///     )
/// })
/// .unwrap();
///
/// ORDER.transition(&db, 1, "paid", Some("stripe")).unwrap();
/// assert_eq!(
///     ORDER.transition(&db, 1, "cancelled", None),
///     Err(Transition::NotAllowed { from: "paid".into(), to: "cancelled".into() })
/// );
/// assert_eq!(ORDER.allowed_from("paid"), ["shipped"]);
///
/// let history = ORDER.history(&db, 1).unwrap();
/// assert_eq!((history[0].from.as_str(), history[0].to.as_str()), ("pending", "paid"));
/// assert_eq!(history[0].by.as_deref(), Some("stripe"));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct States {
    table: &'static str,
    column: &'static str,
    transitions: &'static [(&'static str, &'static str)],
    history: bool,
}

/// One recorded move, oldest first from [`States::history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The state before.
    pub from: String,
    /// The state after.
    pub to: String,
    /// Who or what made the move, when given.
    pub by: Option<String>,
    /// UTC, `YYYY-MM-DD HH:MM:SS`.
    pub at: String,
}

/// Why [`States::transition`] did not move.
#[derive(Debug, PartialEq)]
pub enum Transition {
    /// No row has that `id`.
    NotFound,
    /// `from` → `to` is not one of the allowed moves.
    NotAllowed {
        /// The current state.
        from: String,
        /// The state asked for.
        to: String,
    },
    /// The database failed.
    Database(Error),
}

impl fmt::Display for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "no such row"),
            Self::NotAllowed { from, to } => write!(f, "cannot move from {from} to {to}"),
            Self::Database(error) => write!(f, "database: {error}"),
        }
    }
}

impl std::error::Error for Transition {}

impl From<Error> for Transition {
    fn from(error: Error) -> Self {
        Self::Database(error)
    }
}

impl States {
    /// The `column` of `table` moves only along `transitions`, each a
    /// `(from, to)` pair. The rows need an `id` column.
    pub const fn new(
        table: &'static str,
        column: &'static str,
        transitions: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            table,
            column,
            transitions,
            history: false,
        }
    }

    /// Also records every move in `state_history`, read back with
    /// [`States::history`]. Without it no table is created.
    #[must_use]
    pub const fn with_history(mut self) -> Self {
        self.history = true;
        self
    }

    /// Whether `from` → `to` is allowed.
    pub fn can(&self, from: &str, to: &str) -> bool {
        self.transitions.contains(&(from, to))
    }

    /// The states `from` may move to, in declaration order, such as the
    /// buttons to show.
    pub fn allowed_from(&self, from: &str) -> Vec<&'static str> {
        self.transitions
            .iter()
            .filter(|(start, _)| *start == from)
            .map(|(_, to)| *to)
            .collect()
    }

    /// Moves row `id` to `to`, stamped now, in a transaction of its own; see
    /// [`States::transition_in`].
    pub fn transition(
        &self,
        db: &Db,
        id: i64,
        to: &str,
        by: Option<&str>,
    ) -> Result<(), Transition> {
        db.transaction(|tx| self.transition_in(tx, id, to, by, SystemTime::now(), &[]))
    }

    /// Moves row `id` to `to` inside the caller's transaction, also setting
    /// the `set` columns (such as `("completed_at", &stamp)`), and records
    /// the move with `by` and `at` when history is on. `at` comes from the
    /// app's clock, so a test clock stamps test times. The check, the update
    /// and the history row happen together or not at all.
    pub fn transition_in(
        &self,
        tx: &Tx<'_>,
        id: i64,
        to: &str,
        by: Option<&str>,
        at: SystemTime,
        set: &[(&str, &dyn Param)],
    ) -> Result<(), Transition> {
        let (table, column) = (self.table, self.column);
        savepoint(tx, |connection| {
            let from: String = connection
                .query(
                    &format!("SELECT {column} FROM {table} WHERE id = ?1"),
                    &[&id],
                    |row| row.get(0),
                )?
                .pop()
                .ok_or(Transition::NotFound)?;
            if !self.can(&from, to) {
                return Err(Transition::NotAllowed {
                    from,
                    to: to.to_owned(),
                });
            }
            let columns: String = set
                .iter()
                .map(|(extra, _)| format!(", {} = ?", name(extra)))
                .collect();
            let values: Vec<&dyn Param> = [&to as &dyn Param]
                .into_iter()
                .chain(set.iter().map(|(_, value)| *value))
                .chain([&id as &dyn Param])
                .collect();
            connection.execute(
                &format!("UPDATE {table} SET {column} = ?{columns} WHERE id = ?"),
                &values,
            )?;
            if self.history {
                create_history(connection)?;
                connection.execute(
                    "INSERT INTO state_history (model, model_id, field, from_state, to_state, by, at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![table, id, column, from, to, by, timestamp::format(at)],
                )?;
            }
            Ok(())
        })
    }

    /// Every recorded move of row `id`, oldest first; none without
    /// [`States::with_history`].
    pub fn history(&self, db: &Db, id: i64) -> Result<Vec<Change>> {
        if !self.history {
            return Ok(Vec::new());
        }
        db.with(|connection| {
            create_history(connection)?;
            connection.query(
                "SELECT from_state, to_state, by, at FROM state_history
                 WHERE model = ?1 AND model_id = ?2 AND field = ?3 ORDER BY id",
                params![self.table, id, self.column],
                |row| {
                    Ok(Change {
                        from: row.get(0)?,
                        to: row.get(1)?,
                        by: row.get(2)?,
                        at: row.get(3)?,
                    })
                },
            )
        })
    }
}

fn create_history(connection: &Connection<'_>) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS state_history (
            id INTEGER PRIMARY KEY,
            model TEXT NOT NULL,
            model_id INTEGER NOT NULL,
            field TEXT NOT NULL,
            from_state TEXT NOT NULL,
            to_state TEXT NOT NULL,
            by TEXT,
            at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS state_history_model
            ON state_history (model, model_id, field, id)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const POST: States = States::new(
        "posts",
        "status",
        &[("draft", "published"), ("published", "archived")],
    )
    .with_history();

    fn db() -> Db {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, status TEXT NOT NULL DEFAULT 'draft');
                 INSERT INTO posts DEFAULT VALUES;",
            )
        })
        .unwrap();
        db
    }

    #[test]
    fn refused_moves_change_nothing() {
        let db = db();
        assert!(matches!(
            POST.transition(&db, 1, "archived", None),
            Err(Transition::NotAllowed { .. })
        ));
        assert_eq!(
            POST.transition(&db, 9, "published", None),
            Err(Transition::NotFound)
        );
        assert!(POST.history(&db, 1).unwrap().is_empty());
        let status: String = db
            .with(|sql| sql.query_row("SELECT status FROM posts", &[], |row| row.get(0)))
            .unwrap();
        assert_eq!(status, "draft");
    }

    #[test]
    fn transitions_nest_inside_an_open_transaction() {
        let db = db();
        db.with(|sql| sql.execute_batch("BEGIN")).unwrap();
        POST.transition(&db, 1, "published", None).unwrap();
        db.with(|sql| sql.execute_batch("ROLLBACK")).unwrap();
        assert!(
            POST.history(&db, 1).unwrap().is_empty(),
            "undone with the outer one"
        );
    }

    #[test]
    fn history_follows_every_move() {
        let db = db();
        POST.transition(&db, 1, "published", Some("neo")).unwrap();
        POST.transition(&db, 1, "archived", None).unwrap();
        let moves: Vec<(String, String)> = POST
            .history(&db, 1)
            .unwrap()
            .into_iter()
            .map(|change| (change.from, change.to))
            .collect();
        assert_eq!(
            moves,
            [
                ("draft".into(), "published".into()),
                ("published".into(), "archived".into())
            ]
        );
        assert!(POST.allowed_from("archived").is_empty());
    }

    #[test]
    fn transitions_take_the_apps_time_and_extra_columns() {
        let db = db();
        db.with(|sql| sql.execute_batch("ALTER TABLE posts ADD COLUMN published_at TEXT"))
            .unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let stamp = timestamp::format(at);
        db.transaction(|tx| {
            POST.transition_in(
                tx,
                1,
                "published",
                Some("neo"),
                at,
                &[("published_at", &stamp)],
            )
        })
        .unwrap();
        assert_eq!(POST.history(&db, 1).unwrap()[0].at, "2001-09-09 01:46:40");
        let published: String = db
            .with(|sql| sql.query_row("SELECT published_at FROM posts", &[], |row| row.get(0)))
            .unwrap();
        assert_eq!(published, "2001-09-09 01:46:40");
    }

    #[test]
    fn without_history_no_table_is_created() {
        const QUIET: States = States::new("posts", "status", &[("draft", "published")]);
        let db = db();
        QUIET.transition(&db, 1, "published", None).unwrap();
        assert!(QUIET.history(&db, 1).unwrap().is_empty());
        let tables: i64 = db
            .with(|sql| {
                sql.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'state_history'",
                    &[],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(tables, 0);
    }
}
