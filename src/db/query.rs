//! A query builder, like Laravel's `DB::table`.
//!
//! Values are always bound as parameters. Table and column names are written
//! into the SQL, so they must come from the app's code; a name that is not
//! `letters, digits, _ or .` panics rather than reach SQLite.

use rusqlite::{Connection, Params, Result, Row, ToSql, params_from_iter};

use super::Db;

/// A query on one table, from [`Db::table`]. Chain conditions, then finish
/// with [`get`](Self::get), [`first`](Self::first), [`count`](Self::count),
/// [`insert`](Self::insert), [`update`](Self::update) or
/// [`delete`](Self::delete).
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, sqlite::params};
///
/// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
/// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, views INTEGER)"))
///     .unwrap();
/// let posts = db.table("posts");
/// let id = posts.insert(&["title", "views"], params!["Hello", 3]).unwrap();
/// posts.insert(&["title", "views"], params!["Again", 10]).unwrap();
///
/// let popular = db
///     .table("posts")
///     .where_op("views", ">", &5)
///     .order_by_desc("id")
///     .get(|row| row.get::<_, String>("title"))
///     .unwrap();
/// assert_eq!(popular, ["Again"]);
///
/// db.table("posts").where_eq("id", &id).update(&["views"], params![4]).unwrap();
/// let views = db.table("posts").where_eq("id", &id).first(|row| row.get::<_, i64>("views"));
/// assert_eq!(views.unwrap(), Some(4));
/// assert_eq!(db.table("posts").where_eq("id", &id).delete().unwrap(), 1);
/// assert_eq!(db.table("posts").count().unwrap(), 1);
/// ```
pub struct Query<'a> {
    source: Source<'a>,
    table: String,
    conditions: Vec<String>,
    values: Vec<&'a dyn ToSql>,
    order: Vec<String>,
    limit: Option<u64>,
}

/// Where a query gets its connection: the shared one, locked per call, or the
/// one a transaction already holds.
#[derive(Clone, Copy)]
enum Source<'a> {
    Db(&'a Db),
    Transaction(&'a Connection),
}

impl<'a> Query<'a> {
    pub(super) fn new(db: &'a Db, table: &str) -> Self {
        Self::on(Source::Db(db), table)
    }

    /// A query that runs on `connection`, which the caller already holds.
    pub(super) fn in_transaction(connection: &'a Connection, table: &str) -> Self {
        Self::on(Source::Transaction(connection), table)
    }

    fn on(source: Source<'a>, table: &str) -> Self {
        Self {
            source,
            table: name(table),
            conditions: Vec::new(),
            values: Vec::new(),
            order: Vec::new(),
            limit: None,
        }
    }

    /// Keeps rows where `column` equals `value`.
    pub fn where_eq(self, column: &str, value: &'a dyn ToSql) -> Self {
        self.where_op(column, "=", value)
    }

    /// Keeps rows where `column {operator} value`, with `operator` one of
    /// `=`, `!=`, `<`, `<=`, `>`, `>=` or `LIKE`.
    ///
    /// # Panics
    ///
    /// On any other operator.
    pub fn where_op(mut self, column: &str, operator: &str, value: &'a dyn ToSql) -> Self {
        assert!(
            ["=", "!=", "<", "<=", ">", ">=", "LIKE"].contains(&operator),
            "unsupported operator {operator:?}"
        );
        self.conditions
            .push(format!("{} {operator} ?", name(column)));
        self.values.push(value);
        self
    }

    /// Keeps rows where `column` is `NULL`.
    pub fn where_null(mut self, column: &str) -> Self {
        self.conditions.push(format!("{} IS NULL", name(column)));
        self
    }

    /// Sorts by `column`, smallest first. Later calls break ties.
    pub fn order_by(mut self, column: &str) -> Self {
        self.order.push(name(column));
        self
    }

    /// Sorts by `column`, largest first. Later calls break ties.
    pub fn order_by_desc(mut self, column: &str) -> Self {
        self.order.push(format!("{} DESC", name(column)));
        self
    }

    /// Returns at most `rows` rows.
    pub fn limit(mut self, rows: u64) -> Self {
        self.limit = Some(rows);
        self
    }

    /// Every matching row, each turned into a `T` by `map`. Read columns by
    /// name: `row.get("title")`.
    pub fn get<T>(&self, map: impl FnMut(&Row<'_>) -> Result<T>) -> Result<Vec<T>> {
        let sql = format!(
            "SELECT * FROM {}{}{}{}",
            self.table,
            self.where_sql(),
            self.order_sql(),
            self.limit
                .map(|rows| format!(" LIMIT {rows}"))
                .unwrap_or_default()
        );
        self.with(|connection| {
            connection
                .prepare(&sql)?
                .query_map(params_from_iter(&self.values), map)?
                .collect()
        })
    }

    /// The first matching row, if any.
    pub fn first<T>(self, map: impl FnMut(&Row<'_>) -> Result<T>) -> Result<Option<T>> {
        Ok(self.limit(1).get(map)?.pop())
    }

    /// How many rows match.
    pub fn count(&self) -> Result<i64> {
        let sql = format!("SELECT count(*) FROM {}{}", self.table, self.where_sql());
        self.with(|connection| {
            connection.query_row(&sql, params_from_iter(&self.values), |row| row.get(0))
        })
    }

    /// Inserts one row with `values` for `columns` and returns its `id`.
    /// Conditions are ignored.
    pub fn insert(&self, columns: &[&str], values: impl Params) -> Result<i64> {
        let names: Vec<String> = columns.iter().map(|column| name(column)).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            names.join(", "),
            vec!["?"; columns.len()].join(", ")
        );
        self.with(|connection| {
            connection.execute(&sql, values)?;
            Ok(connection.last_insert_rowid())
        })
    }

    /// Sets `columns` to `values` on every matching row, and returns how many
    /// changed. Without a condition that is every row.
    pub fn update(&self, columns: &[&str], values: &[&dyn ToSql]) -> Result<usize> {
        let sets: Vec<String> = columns
            .iter()
            .map(|column| format!("{} = ?", name(column)))
            .collect();
        let sql = format!(
            "UPDATE {} SET {}{}",
            self.table,
            sets.join(", "),
            self.where_sql()
        );
        let all = values.iter().copied().chain(self.values.iter().copied());
        self.with(|connection| connection.execute(&sql, params_from_iter(all)))
    }

    /// Deletes every matching row, and returns how many. Without a condition
    /// that is every row.
    pub fn delete(&self) -> Result<usize> {
        let sql = format!("DELETE FROM {}{}", self.table, self.where_sql());
        self.with(|connection| connection.execute(&sql, params_from_iter(&self.values)))
    }

    fn with<T>(&self, work: impl FnOnce(&Connection) -> T) -> T {
        match self.source {
            Source::Db(db) => db.with(work),
            Source::Transaction(connection) => work(connection),
        }
    }

    fn where_sql(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.conditions.join(" AND "))
        }
    }

    fn order_sql(&self) -> String {
        if self.order.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", self.order.join(", "))
        }
    }
}

/// `identifier` checked to be a plain name, so it cannot carry SQL.
pub(super) fn name(identifier: &str) -> String {
    assert!(
        !identifier.is_empty()
            && identifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'),
        "not a table or column name: {identifier:?}"
    );
    identifier.to_owned()
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::db::Db;

    #[test]
    #[should_panic(expected = "not a table or column name")]
    fn names_cannot_carry_sql() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let _ = db.table("posts").order_by("id; DROP TABLE posts");
    }

    #[test]
    fn values_are_bound_not_spliced() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| sql.execute_batch("CREATE TABLE posts (title TEXT)"))
            .unwrap();
        let sneaky = "x' OR '1'='1";
        db.table("posts").insert(&["title"], [&"real"]).unwrap();
        assert_eq!(
            db.table("posts")
                .where_eq("title", &sneaky)
                .count()
                .unwrap(),
            0
        );
    }
}
