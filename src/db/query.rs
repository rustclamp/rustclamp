//! A query builder, like Laravel's `DB::table`.
//!
//! Values are always bound as parameters. Table and column names are written
//! into the SQL, so they must come from the app's code; a name that is not
//! `letters, digits, _ or .` panics rather than reach SQLite.

use super::dialect::to_postgres_placeholders;
use super::{Connection, Db, Dialect, Error, Param, Result, Row};

/// The most values one statement binds: SQLite's default limit since 3.32,
/// and under the 32 767 of the Postgres client.
const MAX_PARAMS: usize = 32_766;

/// A query on one table, from [`Db::table`]. Chain conditions, then finish
/// with [`get`](Self::get), [`first`](Self::first), [`count`](Self::count),
/// [`paginate`](Self::paginate), [`insert`](Self::insert),
/// [`insert_many`](Self::insert_many), [`upsert`](Self::upsert),
/// [`update`](Self::update) or [`delete`](Self::delete).
///
/// ```
/// use rustclamp::config::Config;
/// use rustclamp::db::{Db, params};
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
///     .get(|row| row.get::<String>("title"))
///     .unwrap();
/// assert_eq!(popular, ["Again"]);
///
/// db.table("posts").where_eq("id", &id).update(&["views"], params![4]).unwrap();
/// let views = db.table("posts").where_eq("id", &id).first(|row| row.get::<i64>("views"));
/// assert_eq!(views.unwrap(), Some(4));
/// assert_eq!(db.table("posts").where_eq("id", &id).delete().unwrap(), 1);
/// assert_eq!(db.table("posts").count().unwrap(), 1);
/// ```
pub struct Query<'a> {
    source: Source<'a>,
    table: String,
    joins: Vec<String>,
    conditions: Vec<String>,
    values: Vec<&'a dyn Param>,
    order: Vec<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}

/// Where a query gets its connection: the shared one, locked per call, or the
/// one a transaction already holds.
#[derive(Clone, Copy)]
enum Source<'a> {
    Db(&'a Db),
    Transaction(Connection<'a>),
}

impl<'a> Query<'a> {
    pub(super) fn new(db: &'a Db, table: &str) -> Self {
        Self::on(Source::Db(db), table)
    }

    /// A query that runs on `connection`, which the caller already holds.
    pub(super) fn in_transaction(connection: Connection<'a>, table: &str) -> Self {
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
            offset: None,
        }
    }

    /// Keeps rows where `column` equals `value`.
    pub fn where_eq(self, column: &str, value: &'a dyn Param) -> Self {
        self.where_op(column, "=", value)
    }

    /// Keeps rows where `column {operator} value`, with `operator` one of
    /// `=`, `!=`, `<`, `<=`, `>`, `>=` or `LIKE`.
    ///
    /// # Panics
    ///
    /// On any other operator.
    pub fn where_op(mut self, column: &str, operator: &str, value: &'a dyn Param) -> Self {
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
    pub fn where_in(mut self, column: &str, values: &[&'a dyn Param]) -> Self {
        if values.is_empty() {
            // Not a bare `0`: Postgres wants a boolean here.
            self.conditions.push("1 = 0".to_owned());
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
    pub fn where_exists(mut self, select: &'static str, params: &[&'a dyn Param]) -> Self {
        self.conditions.push(format!("EXISTS ({select})"));
        self.values.extend_from_slice(params);
        self
    }

    /// [`where_exists`](Self::where_exists), negated.
    pub fn where_not_exists(mut self, select: &'static str, params: &[&'a dyn Param]) -> Self {
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

    /// Skips the first `rows` rows; pair it with an order so pages are stable.
    pub fn offset(mut self, rows: u64) -> Self {
        self.offset = Some(rows);
        self
    }

    /// Page `page` (from 1; 0 reads as 1) of `per_page` rows, with the total
    /// count for page links. Order the query first, or rows can move between
    /// pages.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::Db;
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY);
    ///     INSERT INTO posts VALUES (1), (2), (3), (4), (5)")).unwrap();
    /// let page = db.table("posts").order_by("id").paginate(2, 2, |row| row.get::<i64>("id")).unwrap();
    /// assert_eq!(page.items, [3, 4]);
    /// assert_eq!((page.total, page.last_page(), page.previous(), page.next()), (5, 3, Some(1), Some(3)));
    /// ```
    pub fn paginate<T>(
        self,
        page: u64,
        per_page: u64,
        map: impl FnMut(&Row) -> Result<T>,
    ) -> Result<Page<T>> {
        let (page, per_page) = (page.max(1), per_page.max(1));
        let total = self.count()?.try_into().unwrap_or(0);
        let items = self
            .limit(per_page)
            .offset((page - 1).saturating_mul(per_page))
            .get(map)?;
        Ok(Page {
            items,
            page,
            per_page,
            total,
        })
    }

    /// Every matching row, each turned into a `T` by `map`. Read columns by
    /// name: `row.get("title")`.
    pub fn get<T>(&self, map: impl FnMut(&Row) -> Result<T>) -> Result<Vec<T>> {
        let sql = self.select_sql(self.dialect());
        self.read(|connection| connection.query(&sql, &self.values, map))
    }

    fn select_sql(&self, dialect: Dialect) -> String {
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
            match (self.limit, self.offset) {
                (None, None) => String::new(),
                (Some(rows), None) => format!(" LIMIT {rows}"),
                (Some(rows), Some(skip)) => format!(" LIMIT {rows} OFFSET {skip}"),
                (None, Some(skip)) => match dialect {
                    // SQLite needs a LIMIT before OFFSET; -1 means none.
                    Dialect::Sqlite => format!(" LIMIT -1 OFFSET {skip}"),
                    Dialect::Postgres => format!(" OFFSET {skip}"),
                },
            }
        );
        for_dialect(sql, dialect)
    }

    /// The first matching row, if any.
    pub fn first<T>(self, map: impl FnMut(&Row) -> Result<T>) -> Result<Option<T>> {
        Ok(self.limit(1).get(map)?.pop())
    }

    /// How many rows match.
    pub fn count(&self) -> Result<i64> {
        let sql = self.count_sql(self.dialect());
        self.read(|connection| connection.query_row(&sql, &self.values, |row| row.get(0)))
    }

    fn count_sql(&self, dialect: Dialect) -> String {
        for_dialect(
            format!(
                "SELECT count(*) FROM {}{}{}",
                self.table,
                self.joins.concat(),
                self.where_sql()
            ),
            dialect,
        )
    }

    /// Inserts one row with `values` for `columns` and returns its `id`.
    /// Conditions are ignored.
    pub fn insert(&self, columns: &[&str], values: &[&dyn Param]) -> Result<i64> {
        self.with(|connection| match connection.dialect() {
            Dialect::Sqlite => {
                connection.execute(&self.insert_sql(columns, Dialect::Sqlite, false), values)?;
                Ok(connection.last_insert_rowid())
            }
            Dialect::Postgres => connection.query_row(
                &self.insert_sql(columns, Dialect::Postgres, true),
                values,
                |row| row.get(0),
            ),
        })
    }

    /// [`insert`](Self::insert) for a table with no `id` column, which has
    /// nothing to return.
    pub fn insert_only(&self, columns: &[&str], values: &[&dyn Param]) -> Result<()> {
        self.with(|connection| {
            connection
                .execute(
                    &self.insert_sql(columns, connection.dialect(), false),
                    values,
                )
                .map(|_| ())
        })
    }

    /// Inserts every row of `rows`, each with values for `columns`, in as few
    /// statements as the engines allow: multi-row `VALUES`, at most 32 766
    /// values a statement. Returns how many rows went in. It runs in one
    /// transaction (the caller's, from [`Tx::table`](super::Tx::table)), so
    /// either every row goes in or none does. Conditions are ignored.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, Param};
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch("CREATE TABLE prices (station TEXT, price REAL)")).unwrap();
    /// let read = [("S1", 1.49), ("S2", 1.52)];
    /// let rows: Vec<[&dyn Param; 2]> = read.iter().map(|(s, p)| [s as &dyn Param, p]).collect();
    /// assert_eq!(db.table("prices").insert_many(&["station", "price"], &rows).unwrap(), 2);
    /// ```
    ///
    /// # Errors
    ///
    /// When a row does not have one value per column, or the SQL fails; then
    /// no row of this call stays.
    pub fn insert_many<'v, R: AsRef<[&'v dyn Param]>>(
        &self,
        columns: &[&str],
        rows: &[R],
    ) -> Result<usize> {
        self.write_many(columns, rows, "")
    }

    /// [`insert_many`](Self::insert_many) as an upsert: a row that would break
    /// the unique key `unique_by` sets the `update` columns on the row already
    /// there, from `excluded`: `INSERT ... VALUES (..), (..) ON CONFLICT
    /// (unique_by) DO UPDATE SET col = excluded.col`, on both engines. With
    /// `update` empty such a row is left alone (`DO NOTHING`).
    ///
    /// With `only_if_changed` columns, an existing row is updated only when
    /// one of them differs: `WHERE t.c1 IS DISTINCT FROM excluded.c1 OR ..`,
    /// so a change to or from `NULL` counts and an unchanged row is not
    /// written (its other `update` columns keep their old values too).
    ///
    /// Returns how many rows went in or changed; a row skipped by
    /// `only_if_changed` or `DO NOTHING` does not count. Same chunks and
    /// transaction as `insert_many`; nothing comes back per row. A key may
    /// appear once per call: Postgres refuses to update a row twice in one
    /// statement.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, Param};
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch(
    ///     "CREATE TABLE prices (station TEXT PRIMARY KEY, price REAL, seen INTEGER)",
    /// )).unwrap();
    /// let columns = ["station", "price", "seen"];
    /// let upsert = |rows: &[[&dyn Param; 3]]| {
    ///     db.table("prices").upsert_many(&columns, rows, &["station"], &["price", "seen"], &["price"])
    /// };
    /// upsert(&[[&"S1", &1.49, &1], [&"S2", &1.52, &1]]).unwrap();
    /// // S1's price is unchanged, so only S2 is written.
    /// assert_eq!(upsert(&[[&"S1", &1.49, &2], [&"S2", &1.55, &2]]).unwrap(), 1);
    /// ```
    ///
    /// # Errors
    ///
    /// As [`insert_many`](Self::insert_many); also when `unique_by` matches
    /// no unique index.
    ///
    /// # Panics
    ///
    /// When `unique_by` is empty, or `only_if_changed` is given with `update`
    /// empty.
    pub fn upsert_many<'v, R: AsRef<[&'v dyn Param]>>(
        &self,
        columns: &[&str],
        rows: &[R],
        unique_by: &[&str],
        update: &[&str],
        only_if_changed: &[&str],
    ) -> Result<usize> {
        let suffix = on_conflict(&self.table, unique_by, update, only_if_changed);
        self.write_many(columns, rows, &suffix)
    }

    /// Multi-row inserts of `rows` in chunks, each followed by `suffix`.
    fn write_many<'v, R: AsRef<[&'v dyn Param]>>(
        &self,
        columns: &[&str],
        rows: &[R],
        suffix: &str,
    ) -> Result<usize> {
        if let Some(at) = rows
            .iter()
            .position(|row| row.as_ref().len() != columns.len())
        {
            return Err(Error::new(format!(
                "row {at} has {} values for {} columns",
                rows[at].as_ref().len(),
                columns.len()
            )));
        }
        let connection = match self.source {
            _ if rows.is_empty() => return Ok(0),
            Source::Db(db) => {
                return db
                    .transaction(|tx| tx.table(&self.table).write_many(columns, rows, suffix));
            }
            Source::Transaction(connection) => connection,
        };
        let mut inserted = 0;
        for chunk in rows.chunks((MAX_PARAMS / columns.len().max(1)).max(1)) {
            let sql = self.insert_many_sql(columns, chunk.len(), suffix, connection.dialect());
            let values: Vec<&dyn Param> =
                chunk.iter().flat_map(|row| row.as_ref()).copied().collect();
            inserted += connection.execute(&sql, &values)?;
        }
        Ok(inserted)
    }

    fn insert_many_sql(
        &self,
        columns: &[&str],
        rows: usize,
        suffix: &str,
        dialect: Dialect,
    ) -> String {
        let names: Vec<String> = columns.iter().map(|column| name(column)).collect();
        let row = format!("({})", vec!["?"; columns.len()].join(", "));
        for_dialect(
            format!(
                "INSERT INTO {} ({}) VALUES {}{suffix}",
                self.table,
                names.join(", "),
                vec![row; rows].join(", ")
            ),
            dialect,
        )
    }

    /// Inserts one row, or, when it would break the unique key `unique_by`,
    /// sets `update` on the row already there to the values given instead,
    /// and returns the row's `id` either way: `INSERT ... ON CONFLICT
    /// (unique_by) DO UPDATE SET ... RETURNING id` on both engines.
    /// `unique_by` must match a primary key or unique index. With `update`
    /// empty an existing row keeps its values (only its `id` comes back).
    /// Conditions are ignored.
    ///
    /// ```
    /// use rustclamp::config::Config;
    /// use rustclamp::db::{Db, params};
    ///
    /// let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
    /// db.with(|sql| sql.execute_batch(
    ///     "CREATE TABLE stations (id INTEGER PRIMARY KEY, code TEXT UNIQUE, name TEXT)",
    /// )).unwrap();
    /// let stations = db.table("stations");
    /// let id = stations.upsert(&["code", "name"], params!["S1", "Old"], &["code"], &["name"]).unwrap();
    /// let again = stations.upsert(&["code", "name"], params!["S1", "New"], &["code"], &["name"]).unwrap();
    /// assert_eq!(id, again);
    /// let name = stations.where_eq("id", &id).first(|row| row.get::<String>("name"));
    /// assert_eq!(name.unwrap().as_deref(), Some("New"));
    /// ```
    ///
    /// # Panics
    ///
    /// When `unique_by` is empty.
    pub fn upsert(
        &self,
        columns: &[&str],
        values: &[&dyn Param],
        unique_by: &[&str],
        update: &[&str],
    ) -> Result<i64> {
        self.with(|connection| {
            let sql = self.upsert_sql(columns, unique_by, update, connection.dialect());
            connection.query_row(&sql, values, |row| row.get(0))
        })
    }

    fn upsert_sql(
        &self,
        columns: &[&str],
        unique_by: &[&str],
        update: &[&str],
        dialect: Dialect,
    ) -> String {
        assert!(!unique_by.is_empty(), "upsert needs a unique key");
        let keys: Vec<String> = unique_by.iter().map(|column| name(column)).collect();
        // `DO NOTHING` would return no row; setting a key to itself returns it unchanged.
        let sets: Vec<String> = if update.is_empty() {
            &unique_by[..1]
        } else {
            update
        }
        .iter()
        .map(|column| format!("{0} = excluded.{0}", name(column)))
        .collect();
        let insert = self.insert_sql(columns, Dialect::Sqlite, false);
        for_dialect(
            format!(
                "{insert} ON CONFLICT ({}) DO UPDATE SET {} RETURNING id",
                keys.join(", "),
                sets.join(", ")
            ),
            dialect,
        )
    }

    /// `returning_id` adds Postgres's `RETURNING id`; SQLite reads
    /// `last_insert_rowid()` instead, which also works without an `id` column.
    fn insert_sql(&self, columns: &[&str], dialect: Dialect, returning_id: bool) -> String {
        let names: Vec<String> = columns.iter().map(|column| name(column)).collect();
        let returning = if returning_id { " RETURNING id" } else { "" };
        for_dialect(
            format!(
                "INSERT INTO {} ({}) VALUES ({}){returning}",
                self.table,
                names.join(", "),
                vec!["?"; columns.len()].join(", ")
            ),
            dialect,
        )
    }

    /// Sets `columns` to `values` on every matching row, and returns how many
    /// changed. Without a condition that is every row.
    pub fn update(&self, columns: &[&str], values: &[&dyn Param]) -> Result<usize> {
        let sql = self.update_sql(columns, self.dialect());
        let all: Vec<&dyn Param> = values.iter().chain(&self.values).copied().collect();
        self.with(|connection| connection.execute(&sql, &all))
    }

    fn update_sql(&self, columns: &[&str], dialect: Dialect) -> String {
        let sets: Vec<String> = columns
            .iter()
            .map(|column| format!("{} = ?", name(column)))
            .collect();
        for_dialect(
            format!(
                "UPDATE {} SET {}{}",
                self.table,
                sets.join(", "),
                self.where_sql()
            ),
            dialect,
        )
    }

    /// [`update`](Self::update) with `(column, value)` pairs, so a column and
    /// its value cannot get out of step.
    pub fn update_values(&self, set: &[(&str, &dyn Param)]) -> Result<usize> {
        let columns: Vec<&str> = set.iter().map(|(column, _)| *column).collect();
        let values: Vec<&dyn Param> = set.iter().map(|(_, value)| *value).collect();
        self.update(&columns, &values)
    }

    /// Deletes every matching row, and returns how many. Without a condition
    /// that is every row.
    pub fn delete(&self) -> Result<usize> {
        let sql = self.delete_sql(self.dialect());
        self.with(|connection| connection.execute(&sql, &self.values))
    }

    fn delete_sql(&self, dialect: Dialect) -> String {
        for_dialect(
            format!("DELETE FROM {}{}", self.table, self.where_sql()),
            dialect,
        )
    }

    fn dialect(&self) -> Dialect {
        match self.source {
            Source::Db(db) => db.dialect(),
            Source::Transaction(connection) => connection.dialect(),
        }
    }

    fn read<T>(&self, work: impl FnOnce(&Connection<'_>) -> T) -> T {
        match self.source {
            Source::Db(db) => db.read(work),
            Source::Transaction(connection) => work(&connection),
        }
    }

    fn with<T>(&self, work: impl FnOnce(&Connection<'_>) -> T) -> T {
        match self.source {
            Source::Db(db) => db.with(work),
            Source::Transaction(connection) => work(&connection),
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

/// One page of rows from [`Query::paginate`].
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    /// The rows on this page.
    pub items: Vec<T>,
    /// This page's number, from 1.
    pub page: u64,
    /// Rows per page.
    pub per_page: u64,
    /// Matching rows across all pages.
    pub total: u64,
}

impl<T> Page<T> {
    /// The last page's number; 1 when there are no rows.
    pub fn last_page(&self) -> u64 {
        self.total.div_ceil(self.per_page).max(1)
    }

    /// The page before this one, if any.
    pub fn previous(&self) -> Option<u64> {
        (self.page > 1).then(|| (self.page - 1).min(self.last_page()))
    }

    /// The page after this one, if any.
    pub fn next(&self) -> Option<u64> {
        (self.page < self.last_page()).then_some(self.page + 1)
    }
}

/// In a view: `items` for `@foreach`, plus `page`, `last_page`, `total`,
/// `previous` and `next` (each `false` when there is none) for links.
#[cfg(feature = "web")]
impl<T: crate::web::ToValue> crate::web::ToValue for Page<T> {
    fn to_value(&self) -> crate::web::Value {
        crate::web::Value::map(&[
            ("items", &self.items as &dyn crate::web::ToValue),
            ("page", &self.page),
            ("last_page", &self.last_page()),
            ("total", &self.total),
            ("previous", &self.previous()),
            ("next", &self.next()),
        ])
    }
}

/// ` ON CONFLICT (unique_by) DO UPDATE SET ..`, with `WHERE` checks for
/// `only_if_changed`, or `DO NOTHING` with nothing to update.
fn on_conflict(
    table: &str,
    unique_by: &[&str],
    update: &[&str],
    only_if_changed: &[&str],
) -> String {
    assert!(!unique_by.is_empty(), "upsert needs a unique key");
    let keys: Vec<String> = unique_by.iter().map(|column| name(column)).collect();
    if update.is_empty() {
        assert!(
            only_if_changed.is_empty(),
            "only_if_changed needs columns to update"
        );
        return format!(" ON CONFLICT ({}) DO NOTHING", keys.join(", "));
    }
    let sets: Vec<String> = update
        .iter()
        .map(|column| format!("{0} = excluded.{0}", name(column)))
        .collect();
    // `IS DISTINCT FROM`, so NULL to a value is a change; SQLite has it since 3.39.
    let changed: Vec<String> = only_if_changed
        .iter()
        .map(|column| format!("{table}.{0} IS DISTINCT FROM excluded.{0}", name(column)))
        .collect();
    let only = if changed.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", changed.join(" OR "))
    };
    format!(
        " ON CONFLICT ({}) DO UPDATE SET {}{only}",
        keys.join(", "),
        sets.join(", ")
    )
}

/// `sql` as the dialect writes it: Postgres numbers its `?` marks.
fn for_dialect(sql: String, dialect: Dialect) -> String {
    match dialect {
        Dialect::Sqlite => sql,
        Dialect::Postgres => to_postgres_placeholders(&sql),
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
    use crate::db::testing::each_engine;

    #[test]
    #[should_panic(expected = "not a table or column name")]
    fn names_cannot_carry_sql() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let _ = db.table("posts").order_by("id; DROP TABLE posts");
    }

    /// Runs `test` on a blog database per engine.
    fn on_blog(test: impl Fn(&Db)) {
        each_engine(|db| {
            blog(db);
            test(db);
        });
    }

    fn blog(db: &Db) {
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, views INTEGER);
                 CREATE TABLE tags (post_id INTEGER, name TEXT);
                 INSERT INTO posts VALUES (1, 'a', 5), (2, 'B', 30), (3, 'c', 10);
                 INSERT INTO tags VALUES (1, 'rust'), (2, 'rust'), (2, 'sql');",
            )
        })
        .unwrap();
    }

    fn titles(query: super::Query<'_>) -> Vec<String> {
        query.get(|row| row.get("title")).unwrap()
    }

    #[test]
    fn where_in_binds_and_empty_matches_nothing() {
        on_blog(|db| {
            let ids: [&dyn crate::db::Param; 2] = [&1, &3];
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
        });
    }

    #[test]
    fn exists_takes_a_subquery_with_bound_params() {
        on_blog(|db| {
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
        });
    }

    #[test]
    fn joins_and_raw_order() {
        on_blog(|db| {
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
        });
    }

    #[test]
    fn update_takes_named_values() {
        on_blog(|db| {
            let changed = db
                .table("posts")
                .where_eq("id", &2)
                .update_values(&[("title", &"z"), ("views", &99)])
                .unwrap();
            assert_eq!(changed, 1);
            let row = db
                .table("posts")
                .where_eq("id", &2)
                .first(|r| Ok((r.get::<String>("title")?, r.get::<i64>("views")?)));
            assert_eq!(row.unwrap(), Some(("z".to_owned(), 99)));
        });
    }

    #[test]
    fn paginate_counts_and_clamps() {
        on_blog(|db| {
            let page = |n, per| {
                db.table("posts")
                    .order_by("id")
                    .paginate(n, per, |row| row.get::<String>("title"))
                    .unwrap()
            };
            let first = page(0, 2);
            assert_eq!(
                (first.items.as_slice(), first.page),
                (&["a".to_owned(), "B".to_owned()][..], 1)
            );
            assert_eq!(
                (first.previous(), first.next(), first.last_page()),
                (None, Some(2), 2)
            );
            let last = page(2, 2);
            assert_eq!((last.next(), last.items), (None, vec!["c".to_owned()]));
            // Past the end: no rows, and "previous" points at the real last page.
            let beyond = page(9, 2);
            assert!(beyond.items.is_empty());
            assert_eq!((beyond.previous(), beyond.total), (Some(2), 3));
            // The count keeps the query's conditions.
            let popular = db
                .table("posts")
                .where_op("views", ">", &6)
                .paginate(1, 1, |row| row.get::<i64>("id"))
                .unwrap();
            assert_eq!((popular.total, popular.last_page()), (2, 2));
            assert_eq!(
                db.table("posts")
                    .order_by("id")
                    .offset(2)
                    .get(|r| r.get::<i64>("id"))
                    .unwrap(),
                [3]
            );
        });
    }

    #[test]
    #[should_panic(expected = "not a table or column name")]
    fn join_names_cannot_carry_sql() {
        on_blog(|db| {
            let _ = db.table("posts").join("tags; DROP TABLE posts", "a", "b");
        });
    }

    #[test]
    fn values_are_bound_not_spliced() {
        each_engine(|db| {
            db.with(|sql| sql.execute_batch("CREATE TABLE posts (title TEXT)"))
                .unwrap();
            let sneaky = "x' OR '1'='1";
            db.table("posts")
                .insert_only(&["title"], &[&"real"])
                .unwrap();
            assert_eq!(
                db.table("posts")
                    .where_eq("title", &sneaky)
                    .count()
                    .unwrap(),
                0
            );
        });
    }

    #[test]
    fn sql_renders_per_dialect() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let query = || {
            db.table("posts")
                .where_eq("a", &1)
                .where_in("b", &[&2, &3])
                .where_null("c")
                .where_exists(
                    "SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = ?",
                    &[&"x"],
                )
                .where_op("d", "LIKE", &"%y%")
                .order_by("id")
        };
        let get = "SELECT * FROM posts WHERE a = ? AND b IN (?, ?) AND c IS NULL AND EXISTS (SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = ?) AND d LIKE ? ORDER BY id";
        assert_eq!(query().select_sql(Sqlite), get);
        assert_eq!(
            query().select_sql(Postgres),
            "SELECT * FROM posts WHERE a = $1 AND b IN ($2, $3) AND c IS NULL AND EXISTS (SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = $4) AND d LIKE $5 ORDER BY id"
        );
        assert_eq!(
            query().count_sql(Postgres),
            "SELECT count(*) FROM posts WHERE a = $1 AND b IN ($2, $3) AND c IS NULL AND EXISTS (SELECT 1 FROM tags WHERE tags.post_id = posts.id AND tags.name = $4) AND d LIKE $5"
        );
        assert_eq!(
            db.table("posts")
                .where_eq("id", &1)
                .update_sql(&["a", "b"], Postgres),
            "UPDATE posts SET a = $1, b = $2 WHERE id = $3"
        );
        assert_eq!(
            db.table("posts").where_eq("id", &1).delete_sql(Postgres),
            "DELETE FROM posts WHERE id = $1"
        );
        assert_eq!(
            db.table("posts").where_in("id", &[]).select_sql(Postgres),
            "SELECT * FROM posts WHERE 1 = 0"
        );
    }

    #[test]
    fn insert_returns_the_id_only_on_postgres() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let posts = db.table("posts");
        assert_eq!(
            posts.insert_sql(&["title", "views"], Sqlite, false),
            "INSERT INTO posts (title, views) VALUES (?, ?)"
        );
        assert_eq!(
            posts.insert_sql(&["title", "views"], Postgres, true),
            "INSERT INTO posts (title, views) VALUES ($1, $2) RETURNING id"
        );
        assert_eq!(
            posts.insert_sql(&["title"], Postgres, false),
            "INSERT INTO posts (title) VALUES ($1)"
        );
    }

    #[test]
    fn offset_without_limit_per_dialect() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let offset = || db.table("posts").offset(5);
        assert_eq!(
            offset().select_sql(Sqlite),
            "SELECT * FROM posts LIMIT -1 OFFSET 5"
        );
        assert_eq!(
            offset().select_sql(Postgres),
            "SELECT * FROM posts OFFSET 5"
        );
        assert_eq!(
            offset().limit(2).select_sql(Sqlite),
            "SELECT * FROM posts LIMIT 2 OFFSET 5"
        );
        assert_eq!(
            offset().limit(2).select_sql(Postgres),
            "SELECT * FROM posts LIMIT 2 OFFSET 5"
        );
        assert_eq!(
            db.table("posts").limit(2).select_sql(Postgres),
            "SELECT * FROM posts LIMIT 2"
        );
    }

    #[test]
    fn upsert_sql_per_dialect() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let prices = db.table("prices");
        assert_eq!(
            prices.upsert_sql(
                &["station", "day", "price"],
                &["station", "day"],
                &["price"],
                Postgres
            ),
            "INSERT INTO prices (station, day, price) VALUES ($1, $2, $3) ON CONFLICT (station, day) DO UPDATE SET price = excluded.price RETURNING id"
        );
        assert_eq!(
            prices.upsert_sql(&["code"], &["code"], &[], Sqlite),
            "INSERT INTO prices (code) VALUES (?) ON CONFLICT (code) DO UPDATE SET code = excluded.code RETURNING id"
        );
    }

    #[test]
    #[should_panic(expected = "upsert needs a unique key")]
    fn upsert_needs_a_key() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let _ = db.table("t").upsert(&["a"], &[&1], &[], &["a"]);
    }

    #[test]
    fn upsert_inserts_then_updates_and_returns_the_same_id() {
        each_engine(|db| {
            let create = crate::db::Schema::create("stations", |table| {
                table.id();
                table.string("code").unique();
                table.string("name");
            });
            db.with(|sql| sql.execute_batch(create.render(db.dialect())))
                .unwrap();
            let stations = db.table("stations");
            let upsert = |code: &str, name: &str, update: &[&str]| {
                stations.upsert(&["code", "name"], &[&code, &name], &["code"], update)
            };
            let first = upsert("a", "one", &["name"]).unwrap();
            let other = upsert("b", "two", &["name"]).unwrap();
            assert_ne!(first, other);
            assert_eq!(upsert("a", "uno", &["name"]).unwrap(), first);
            // With nothing to update the row keeps its name, and its id still comes back.
            assert_eq!(upsert("a", "ignored", &[]).unwrap(), first);
            let name = db
                .table("stations")
                .where_eq("id", &first)
                .first(|r| r.get::<String>("name"));
            assert_eq!(name.unwrap().as_deref(), Some("uno"));
            assert_eq!(stations.count().unwrap(), 2);
            // In a transaction, and a key that matches no unique index fails.
            let in_tx = db.transaction(|tx| {
                tx.table("stations").upsert(
                    &["code", "name"],
                    &[&"c", &"three"],
                    &["code"],
                    &["name"],
                )
            });
            assert!(in_tx.unwrap() > other);
            let no_index = stations.upsert(&["code", "name"], &[&"d", &"x"], &["name"], &["code"]);
            assert!(no_index.is_err());
        });
    }

    #[test]
    fn upsert_many_sql_per_dialect() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let suffix = super::on_conflict(
            "prices",
            &["series_id", "valid_from"],
            &["price", "fetched_at"],
            &["price", "fetched_at"],
        );
        assert_eq!(
            db.table("prices").insert_many_sql(
                &["series_id", "valid_from", "price", "fetched_at"],
                2,
                &suffix,
                Postgres
            ),
            "INSERT INTO prices (series_id, valid_from, price, fetched_at) VALUES ($1, $2, $3, $4), ($5, $6, $7, $8) \
             ON CONFLICT (series_id, valid_from) DO UPDATE SET price = excluded.price, fetched_at = excluded.fetched_at \
             WHERE prices.price IS DISTINCT FROM excluded.price OR prices.fetched_at IS DISTINCT FROM excluded.fetched_at"
        );
        assert_eq!(
            db.table("t").insert_many_sql(
                &["k"],
                1,
                &super::on_conflict("t", &["k"], &[], &[]),
                Sqlite
            ),
            "INSERT INTO t (k) VALUES (?) ON CONFLICT (k) DO NOTHING"
        );
    }

    #[test]
    #[should_panic(expected = "only_if_changed needs columns to update")]
    fn upsert_many_only_if_changed_needs_an_update() {
        let _ = super::on_conflict("t", &["k"], &[], &["v"]);
    }

    #[test]
    fn upsert_many_updates_only_changed_rows_in_chunks() {
        use crate::db::Param;
        each_engine(|db| {
            db.with(|sql| {
                sql.execute_batch(
                    "CREATE TABLE prices (series_id INTEGER, valid_from TEXT, price DOUBLE PRECISION, fetched_at INTEGER,
                     PRIMARY KEY (series_id, valid_from))",
                )
            })
            .unwrap();
            let columns = ["series_id", "valid_from", "price", "fetched_at"];
            let upsert = |price: &dyn Fn(i64) -> f64, fetched: i64, only: &[&str]| {
                // 4 columns: 20 000 rows are three statements.
                let data: Vec<(i64, f64)> = (0..20_000).map(|n| (n, price(n))).collect();
                let rows: Vec<[&dyn Param; 4]> = data
                    .iter()
                    .map(|(n, p)| [n as &dyn Param, &"2026-10-21", p, &fetched])
                    .collect();
                db.table("prices").upsert_many(
                    &columns,
                    &rows,
                    &["series_id", "valid_from"],
                    &["price", "fetched_at"],
                    only,
                )
            };
            assert_eq!(upsert(&|_| 1.0, 1, &[]).unwrap(), 20_000);
            // Every 10th price changes: only those rows are written, and only they get fetched_at 2.
            let changed = upsert(&|n| if n % 10 == 0 { 2.0 } else { 1.0 }, 2, &["price"]);
            assert_eq!(changed.unwrap(), 2_000);
            let fetched: Vec<i64> = db
                .table("prices")
                .where_eq("fetched_at", &2)
                .get(|row| row.get("series_id"))
                .unwrap();
            assert_eq!(fetched.len(), 2_000);
            assert!(fetched.iter().all(|n| n % 10 == 0));
            // Without `only_if_changed` every row is updated; with nothing to update, nothing is.
            assert_eq!(upsert(&|_| 3.0, 3, &[]).unwrap(), 20_000);
            let rows: [[&dyn Param; 4]; 2] = [
                [&1, &"2026-10-21", &9.0, &9],
                [&20_001, &"2026-10-21", &9.0, &9],
            ];
            let nothing = db.table("prices").upsert_many(
                &columns,
                &rows,
                &["series_id", "valid_from"],
                &[],
                &[],
            );
            assert_eq!(nothing.unwrap(), 1);
            assert_eq!(db.table("prices").count().unwrap(), 20_001);
            let kept = db
                .table("prices")
                .where_eq("series_id", &1)
                .first(|row| row.get::<f64>("price"));
            assert_eq!(kept.unwrap(), Some(3.0));
            // A key that matches no unique index fails, and nothing of the call stays.
            let wrong =
                db.table("prices")
                    .upsert_many(&columns, &rows, &["price"], &["fetched_at"], &[]);
            assert!(wrong.is_err());
            assert_eq!(db.table("prices").count().unwrap(), 20_001);
        });
    }

    #[test]
    fn upsert_many_counts_null_changes() {
        use crate::db::Param;
        each_engine(|db| {
            db.with(|sql| {
                sql.execute_batch(
                    "CREATE TABLE prices (id INTEGER PRIMARY KEY, price DOUBLE PRECISION, seen INTEGER)",
                )
            })
            .unwrap();
            let upsert = |price: Option<f64>, seen: i64| {
                let rows: [[&dyn Param; 3]; 1] = [[&1, &price, &seen]];
                db.table("prices").upsert_many(
                    &["id", "price", "seen"],
                    &rows,
                    &["id"],
                    &["price", "seen"],
                    &["price"],
                )
            };
            let state = || {
                db.table("prices")
                    .first(|row| Ok((row.get::<Option<f64>>("price")?, row.get::<i64>("seen")?)))
                    .unwrap()
            };
            assert_eq!(upsert(None, 1).unwrap(), 1);
            // NULL to NULL is no change: `<>` would say unknown, IS DISTINCT FROM says equal.
            assert_eq!(upsert(None, 2).unwrap(), 0);
            assert_eq!(state(), Some((None, 1)));
            // NULL to a value, and a value back to NULL, are changes.
            assert_eq!(upsert(Some(1.5), 3).unwrap(), 1);
            assert_eq!(state(), Some((Some(1.5), 3)));
            assert_eq!(upsert(Some(1.5), 4).unwrap(), 0);
            assert_eq!(upsert(None, 5).unwrap(), 1);
            assert_eq!(state(), Some((None, 5)));
        });
    }

    #[test]
    fn insert_many_sql_per_dialect() {
        use crate::db::Dialect::{Postgres, Sqlite};
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let pairs = db.table("pairs");
        assert_eq!(
            pairs.insert_many_sql(&["a", "b"], 2, "", Sqlite),
            "INSERT INTO pairs (a, b) VALUES (?, ?), (?, ?)"
        );
        assert_eq!(
            pairs.insert_many_sql(&["a", "b"], 2, "", Postgres),
            "INSERT INTO pairs (a, b) VALUES ($1, $2), ($3, $4)"
        );
    }

    #[test]
    fn insert_many_chunks_under_the_parameter_limit_and_is_all_or_nothing() {
        use crate::db::Param;
        each_engine(|db| {
            db.with(|sql| {
                sql.execute_batch("CREATE TABLE pairs (a INTEGER PRIMARY KEY, b TEXT NOT NULL)")
            })
            .unwrap();
            let pairs = || db.table("pairs");
            // 2 columns: three statements of up to 16 383 rows each.
            let numbers: Vec<i64> = (0..40_000).collect();
            let rows: Vec<[&dyn Param; 2]> =
                numbers.iter().map(|n| [n as &dyn Param, &"x"]).collect();
            assert_eq!(pairs().insert_many(&["a", "b"], &rows).unwrap(), 40_000);
            assert_eq!(pairs().count().unwrap(), 40_000);
            let none: &[[&dyn Param; 2]] = &[];
            assert_eq!(pairs().insert_many(&["a", "b"], none).unwrap(), 0);
            // A duplicate in the last chunk undoes the chunks before it.
            let more: Vec<i64> = (40_000..80_000).chain([0]).collect();
            let rows: Vec<[&dyn Param; 2]> = more.iter().map(|n| [n as &dyn Param, &"y"]).collect();
            let error = pairs().insert_many(&["a", "b"], &rows).unwrap_err();
            assert!(error.is_unique_violation(), "{error}");
            assert_eq!(pairs().count().unwrap(), 40_000);
            // A short row is refused before anything runs.
            let short: [&[&dyn Param]; 2] = [&[&90_000, &"z"], &[&90_001]];
            let error = pairs().insert_many(&["a", "b"], &short).unwrap_err();
            assert_eq!(error.to_string(), "row 1 has 1 values for 2 columns");
            // Inside a transaction it is that transaction's.
            let rolled_back: crate::db::Result<()> = db.transaction(|tx| {
                tx.table("pairs")
                    .insert_many(&["a", "b"], &[[&90_000 as &dyn Param, &"t"]])?;
                assert_eq!(tx.table("pairs").count()?, 40_001);
                Err(crate::db::Error::new("undo"))
            });
            assert!(rolled_back.is_err());
            assert_eq!(pairs().count().unwrap(), 40_000);
        });
    }

    #[test]
    fn insert_only_needs_no_id_column() {
        each_engine(|db| {
            db.with(|sql| sql.execute_batch("CREATE TABLE pairs (a INTEGER, b INTEGER)"))
                .unwrap();
            db.table("pairs")
                .insert_only(&["a", "b"], crate::db::params![1, 2])
                .unwrap();
            assert_eq!(db.table("pairs").count().unwrap(), 1);
        });
    }

    #[test]
    fn insert_returns_the_new_id_on_both_engines() {
        each_engine(|db| {
            let create = crate::db::Schema::create("things", |table| {
                table.id();
                table.string("name");
            });
            db.with(|sql| sql.execute_batch(create.render(db.dialect())))
                .unwrap();
            let things = db.table("things");
            assert_eq!(things.insert(&["name"], &[&"a"]).unwrap(), 1);
            assert_eq!(things.insert(&["name"], &[&"b"]).unwrap(), 2);
            let in_tx = db.transaction(|tx| tx.table("things").insert(&["name"], &[&"c"]));
            assert_eq!(in_tx.unwrap(), 3);
        });
    }
}
