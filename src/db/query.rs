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
    joins: Vec<String>,
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
            joins: Vec::new(),
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

    /// Keeps rows where `column` is one of `values`. An empty list keeps
    /// nothing.
    pub fn where_in(mut self, column: &str, values: &[&'a dyn ToSql]) -> Self {
        if values.is_empty() {
            self.conditions.push("0".to_owned());
        } else {
            let marks = vec!["?"; values.len()].join(", ");
            self.conditions
                .push(format!("{} IN ({marks})", name(column)));
            self.values.extend_from_slice(values);
        }
        self
    }

    /// Keeps rows for which the subquery `select` returns a row, such as
    /// `"SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = ?"`.
    /// `params` bind its `?` marks, in order.
    ///
    /// `select` is written into the SQL as is, so it is a `&'static str`: it
    /// must come from the app's code, and anything from a request goes in
    /// `params`.
    pub fn where_exists(mut self, select: &'static str, params: &[&'a dyn ToSql]) -> Self {
        self.conditions.push(format!("EXISTS ({select})"));
        self.values.extend_from_slice(params);
        self
    }

    /// [`where_exists`](Self::where_exists), negated.
    pub fn where_not_exists(mut self, select: &'static str, params: &[&'a dyn ToSql]) -> Self {
        self.conditions.push(format!("NOT EXISTS ({select})"));
        self.values.extend_from_slice(params);
        self
    }

    /// `INNER JOIN table ON left = right`, with `left` and `right` written
    /// `table.column`. With a join, [`get`](Self::get) and
    /// [`first`](Self::first) read only this query's own table's columns, so
    /// a [`Model`](trait@super::Model) still maps; name the joined table in a
    /// condition or [`order_by_raw`](Self::order_by_raw) to use its columns.
    /// Joins apply to reads; SQLite has no joined `UPDATE` or `DELETE`.
    pub fn join(self, table: &str, left: &str, right: &str) -> Self {
        self.joined("INNER", table, left, right)
    }

    /// [`join`](Self::join) as a `LEFT JOIN`: rows without a match stay.
    pub fn left_join(self, table: &str, left: &str, right: &str) -> Self {
        self.joined("LEFT", table, left, right)
    }

    fn joined(mut self, kind: &str, table: &str, left: &str, right: &str) -> Self {
        self.joins.push(format!(
            " {kind} JOIN {} ON {} = {}",
            name(table),
            name(left),
            name(right)
        ));
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

    /// Sorts by an SQL expression, such as `"lower(title) DESC"` or
    /// `"score / views"`. It is written into the SQL as is and not checked, so
    /// it is a `&'static str`: it must come from the app's code. For a column
    /// picked by a request, match it against a fixed list and use
    /// [`order_by`](Self::order_by), which checks the name.
    pub fn order_by_raw(mut self, expression: &'static str) -> Self {
        self.order.push(expression.to_owned());
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
            "SELECT {} FROM {}{}{}{}{}",
            if self.joins.is_empty() {
                "*".to_owned()
            } else {
                format!("{}.*", self.table)
            },
            self.table,
            self.joins.concat(),
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
        let sql = format!(
            "SELECT count(*) FROM {}{}{}",
            self.table,
            self.joins.concat(),
            self.where_sql()
        );
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

    /// [`update`](Self::update) with `(column, value)` pairs, so a column and
    /// its value cannot get out of step.
    pub fn update_values(&self, set: &[(&str, &dyn ToSql)]) -> Result<usize> {
        let columns: Vec<&str> = set.iter().map(|(column, _)| *column).collect();
        let values: Vec<&dyn ToSql> = set.iter().map(|(_, value)| *value).collect();
        self.update(&columns, &values)
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

    fn blog() -> Db {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, views INTEGER);
                 CREATE TABLE tags (post_id INTEGER, name TEXT);
                 INSERT INTO posts VALUES (1, 'a', 5), (2, 'B', 30), (3, 'c', 10);
                 INSERT INTO tags VALUES (1, 'rust'), (2, 'rust'), (2, 'sql');",
            )
        })
        .unwrap();
        db
    }

    fn titles(query: super::Query<'_>) -> Vec<String> {
        query.get(|row| row.get("title")).unwrap()
    }

    #[test]
    fn where_in_binds_and_empty_matches_nothing() {
        let db = blog();
        let ids: [&dyn rusqlite::ToSql; 2] = [&1, &3];
        assert_eq!(titles(db.table("posts").where_in("id", &ids)), ["a", "c"]);
        assert_eq!(db.table("posts").where_in("id", &[]).count().unwrap(), 0);
        let quote = "1) OR (1=1";
        assert_eq!(
            db.table("posts")
                .where_in("title", &[&quote])
                .count()
                .unwrap(),
            0
        );
        // Later conditions keep their own values after the list.
        assert_eq!(
            titles(
                db.table("posts")
                    .where_in("id", &ids)
                    .where_op("views", ">", &6)
            ),
            ["c"]
        );
    }

    #[test]
    fn exists_takes_a_subquery_with_bound_params() {
        let db = blog();
        let tagged = "SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = ?";
        assert_eq!(
            titles(db.table("posts").where_exists(tagged, &[&"sql"])),
            ["B"]
        );
        assert_eq!(
            titles(
                db.table("posts")
                    .where_not_exists(tagged, &[&"rust"])
                    .order_by("id")
            ),
            ["c"]
        );
        assert_eq!(
            titles(
                db.table("posts")
                    .where_exists(tagged, &[&"rust"])
                    .where_op("views", ">", &10)
            ),
            ["B"]
        );
    }

    #[test]
    fn joins_and_raw_order() {
        let db = blog();
        let joined = || db.table("posts").join("tags", "tags.post_id", "posts.id");
        assert_eq!(titles(joined().where_eq("tags.name", &"sql")), ["B"]);
        assert_eq!(joined().count().unwrap(), 3);
        let all = db
            .table("posts")
            .left_join("tags", "tags.post_id", "posts.id")
            .where_null("tags.name");
        assert_eq!(titles(all), ["c"]);
        assert_eq!(
            titles(db.table("posts").order_by_raw("lower(title) DESC")),
            ["c", "B", "a"]
        );
    }

    #[test]
    fn update_takes_named_values() {
        let db = blog();
        let changed = db
            .table("posts")
            .where_eq("id", &2)
            .update_values(&[("title", &"z"), ("views", &99)])
            .unwrap();
        assert_eq!(changed, 1);
        let row = db
            .table("posts")
            .where_eq("id", &2)
            .first(|r| Ok((r.get::<_, String>("title")?, r.get::<_, i64>("views")?)));
        assert_eq!(row.unwrap(), Some(("z".to_owned(), 99)));
    }

    #[test]
    #[should_panic(expected = "not a table or column name")]
    fn join_names_cannot_carry_sql() {
        let db = blog();
        let _ = db.table("posts").join("tags; DROP TABLE posts", "a", "b");
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
