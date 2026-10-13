//! The database types an app sees: [`Connection`], [`Row`], [`Value`],
//! [`Param`], [`FromColumn`] and [`Error`]. They name no engine, so the same
//! app code can run on another one (ADR 0034).

use std::rc::Rc;

use super::Dialect;
#[cfg(feature = "postgres")]
use super::dialect::to_postgres_placeholders;

/// A database value, as bound to a statement or read from a column.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A whole number.
    Integer(i64),
    /// A floating-point number.
    Real(f64),
    /// Text.
    Text(String),
    /// Bytes.
    Blob(Vec<u8>),
    /// A boolean. SQLite binds it as the integer `0` or `1`.
    Bool(bool),
}

/// A database error: the engine's message, or why a column could not be read
/// as the type asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
    /// Postgres's five-character SQLSTATE; `None` for SQLite and local errors.
    sqlstate: Option<String>,
    /// SQLite's extended result code; `None` for Postgres and local errors.
    sqlite_code: Option<i32>,
}

impl Error {
    /// An error with `message`, for a [`FromColumn`] or [`Model`](super::Model)
    /// that rejects what it read.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            sqlstate: None,
            sqlite_code: None,
        }
    }

    /// The SQLSTATE (`23505` for a unique violation) when Postgres raised
    /// this error; `None` for SQLite, which has result codes instead.
    pub fn sqlstate(&self) -> Option<&str> {
        self.sqlstate.as_deref()
    }

    /// Whether a unique or primary key constraint refused the statement, on
    /// either engine: SQLite's `SQLITE_CONSTRAINT_UNIQUE` and `_PRIMARYKEY`,
    /// Postgres's `23505`.
    pub fn is_unique_violation(&self) -> bool {
        self.sqlstate() == Some("23505")
            || matches!(
                self.sqlite_code,
                Some(
                    rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                )
            )
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        let sqlite_code = match &error {
            rusqlite::Error::SqliteFailure(failure, _) => Some(failure.extended_code),
            _ => None,
        };
        Self {
            sqlite_code,
            ..Self::new(error.to_string())
        }
    }
}

/// A database result.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A value that can be bound to a `?` or `?N` mark. Implement it to store
/// your own type; [`params!`](crate::db::params) collects a list of them.
pub trait Param {
    /// The value to bind.
    fn to_value(&self) -> Value;
}

impl<T: Param + ?Sized> Param for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl<T: Param> Param for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, Param::to_value)
    }
}

impl Param for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

impl Param for i64 {
    fn to_value(&self) -> Value {
        Value::Integer(*self)
    }
}

macro_rules! small_integers {
    ($($type:ty),*) => {$(
        impl Param for $type {
            fn to_value(&self) -> Value {
                Value::Integer(i64::from(*self))
            }
        }

        impl FromColumn for $type {
            fn from_value(value: Value) -> Result<Self> {
                let n = i64::from_value(value)?;
                n.try_into()
                    .map_err(|_| Error::new(format!("{n} does not fit in {}", stringify!($type))))
            }
        }
    )*};
}

small_integers!(i32, u32);

impl Param for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

impl Param for f64 {
    fn to_value(&self) -> Value {
        Value::Real(*self)
    }
}

impl Param for str {
    fn to_value(&self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl Param for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl Param for Vec<u8> {
    fn to_value(&self) -> Value {
        Value::Blob(self.clone())
    }
}

/// Stored as its hyphenated text, so it reads well in the database and in
/// `public_id` columns.
impl Param for crate::uuid::Uuid {
    fn to_value(&self) -> Value {
        Value::Text(self.to_string())
    }
}

/// A type a column can be read as, with [`Row::get`].
pub trait FromColumn: Sized {
    /// `value` as `Self`, or why it is not one.
    ///
    /// # Errors
    ///
    /// When `value` has another type, or is out of range.
    fn from_value(value: Value) -> Result<Self>;
}

fn mismatch<T>(value: &Value, wanted: &str) -> Result<T> {
    Err(Error::new(format!("expected {wanted}, found {value:?}")))
}

impl FromColumn for Value {
    fn from_value(value: Value) -> Result<Self> {
        Ok(value)
    }
}

impl<T: FromColumn> FromColumn for Option<T> {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Null => Ok(None),
            value => T::from_value(value).map(Some),
        }
    }
}

impl FromColumn for i64 {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Integer(n) => Ok(n),
            value => mismatch(&value, "an integer"),
        }
    }
}

/// A boolean, or an integer: `0` is false, any other integer true.
impl FromColumn for bool {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Bool(flag) => Ok(flag),
            value => i64::from_value(value).map(|n| n != 0),
        }
    }
}

impl FromColumn for f64 {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Real(n) => Ok(n),
            // As SQLite reads it: an integer column holding a whole number.
            Value::Integer(n) => Ok(n as f64),
            value => mismatch(&value, "a number"),
        }
    }
}

impl FromColumn for String {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Text(text) => Ok(text),
            value => mismatch(&value, "text"),
        }
    }
}

impl FromColumn for Vec<u8> {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Blob(bytes) => Ok(bytes),
            Value::Text(text) => Ok(text.into_bytes()),
            value => mismatch(&value, "bytes"),
        }
    }
}

impl FromColumn for crate::uuid::Uuid {
    fn from_value(value: Value) -> Result<Self> {
        let text = String::from_value(value)?;
        Self::parse(&text).ok_or_else(|| Error::new(format!("not a UUID: {text}")))
    }
}

/// A column of a [`Row`]: its position from 0, or its name.
pub trait ColumnIndex {
    /// The position among `columns`, if there is one.
    fn position(&self, columns: &[String]) -> Option<usize>;
}

impl ColumnIndex for usize {
    fn position(&self, columns: &[String]) -> Option<usize> {
        (*self < columns.len()).then_some(*self)
    }
}

/// Names compare without regard to ASCII case, as SQL does.
impl ColumnIndex for &str {
    fn position(&self, columns: &[String]) -> Option<usize> {
        columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(self))
    }
}

/// One result row.
#[derive(Debug)]
pub struct Row {
    columns: Rc<[String]>,
    values: Vec<Value>,
}

impl Row {
    /// The column `column` (a name or a position) as a `T`.
    ///
    /// # Errors
    ///
    /// When there is no such column, or its value is not a `T`.
    pub fn get<T: FromColumn>(&self, column: impl ColumnIndex) -> Result<T> {
        let Some(at) = column.position(&self.columns) else {
            return Err(Error::new(format!("no such column in {:?}", self.columns)));
        };
        T::from_value(self.values[at].clone())
            .map_err(|problem| Error::new(format!("column {}: {problem}", self.columns[at])))
    }
}

/// The connection [`Db::with`](super::Db::with), [`Db::read`](super::Db::read)
/// and a [`Tx`](super::Tx) lend. SQL marks are `?` (in order) or `?N` (the
/// `N`th value, from 1), on either engine.
#[derive(Clone, Copy)]
pub struct Connection<'a>(pub(super) Inner<'a>);

/// What a [`Connection`] lends: a SQLite connection, a Postgres client (behind
/// a `RefCell`, since the wire client needs `&mut`), or the reason there is none.
#[derive(Clone, Copy)]
pub(super) enum Inner<'a> {
    Sqlite(&'a rusqlite::Connection),
    #[cfg(feature = "postgres")]
    Postgres(&'a std::cell::RefCell<super::postgres::Client>),
    /// The pool could not lend a client; every call returns this error.
    #[cfg(feature = "postgres")]
    Unavailable(&'a Error),
}

impl std::fmt::Debug for Connection<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl rusqlite::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::ValueRef;
        Ok(rusqlite::types::ToSqlOutput::Borrowed(match self {
            Self::Null => ValueRef::Null,
            Self::Integer(n) => ValueRef::Integer(*n),
            Self::Real(n) => ValueRef::Real(*n),
            Self::Text(text) => ValueRef::Text(text.as_bytes()),
            Self::Blob(bytes) => ValueRef::Blob(bytes),
            Self::Bool(flag) => ValueRef::Integer((*flag).into()),
        }))
    }
}

fn bind(params: &[&dyn Param]) -> impl rusqlite::Params {
    rusqlite::params_from_iter(params.iter().map(|param| param.to_value()))
}

#[cfg(feature = "postgres")]
impl From<super::postgres::Error> for Error {
    fn from(error: super::postgres::Error) -> Self {
        use super::postgres::Error::{Connection, Server};
        match error {
            Server(server) => Self {
                sqlstate: Some(server.code.clone()),
                ..Self::new(Server(server).to_string())
            },
            Connection(message) => Self::new(message),
        }
    }
}

#[cfg(feature = "postgres")]
impl From<Value> for super::postgres::Datum {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Integer(n) => Self::Integer(n),
            Value::Real(n) => Self::Real(n),
            Value::Text(text) => Self::Text(text),
            Value::Blob(bytes) => Self::Bytes(bytes),
            Value::Bool(flag) => Self::Bool(flag),
        }
    }
}

#[cfg(feature = "postgres")]
impl From<super::postgres::Datum> for Value {
    fn from(datum: super::postgres::Datum) -> Self {
        use super::postgres::Datum;
        match datum {
            Datum::Null => Self::Null,
            Datum::Integer(n) => Self::Integer(n),
            Datum::Real(n) => Self::Real(n),
            Datum::Text(text) => Self::Text(text),
            Datum::Bytes(bytes) => Self::Blob(bytes),
            Datum::Bool(flag) => Self::Bool(flag),
        }
    }
}

impl Connection<'_> {
    /// The engine this connection talks to.
    pub fn dialect(&self) -> Dialect {
        match self.0 {
            Inner::Sqlite(_) => Dialect::Sqlite,
            #[cfg(feature = "postgres")]
            Inner::Postgres(_) | Inner::Unavailable(_) => Dialect::Postgres,
        }
    }

    /// Whether this Postgres connection's transaction failed and can only be
    /// rolled back; SQLite keeps going after a failed statement.
    pub(super) fn aborted(&self) -> bool {
        match self.0 {
            #[cfg(feature = "postgres")]
            Inner::Postgres(client) => client.borrow().aborted(),
            _ => false,
        }
    }

    /// The id of the last row inserted on this SQLite connection.
    pub(super) fn last_insert_rowid(&self) -> i64 {
        match self.0 {
            Inner::Sqlite(connection) => connection.last_insert_rowid(),
            #[cfg(feature = "postgres")]
            _ => unreachable!("Postgres inserts read RETURNING id"),
        }
    }

    /// Runs one statement and returns how many rows it changed.
    ///
    /// # Errors
    ///
    /// When the SQL fails.
    pub fn execute(&self, sql: &str, params: &[&dyn Param]) -> Result<usize> {
        match self.0 {
            Inner::Sqlite(connection) => Ok(connection.execute(sql, bind(params))?),
            #[cfg(feature = "postgres")]
            _ => Ok(self.run(sql, params)?.affected as usize),
        }
    }

    /// Runs any number of `;`-separated statements, without parameters.
    ///
    /// # Errors
    ///
    /// When any of them fails; the ones before it stay done.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        match self.0 {
            Inner::Sqlite(connection) => Ok(connection.execute_batch(sql)?),
            #[cfg(feature = "postgres")]
            Inner::Postgres(client) => Ok(client.borrow_mut().execute_batch(sql)?),
            #[cfg(feature = "postgres")]
            Inner::Unavailable(error) => Err(error.clone()),
        }
    }

    /// Every row `sql` returns, each turned into a `T` by `map`. For "the
    /// row, if there is one", take `.pop()` of a query that returns at most
    /// one.
    ///
    /// # Errors
    ///
    /// When the SQL or `map` fails.
    pub fn query<T>(
        &self,
        sql: &str,
        params: &[&dyn Param],
        mut map: impl FnMut(&Row) -> Result<T>,
    ) -> Result<Vec<T>> {
        let mut all = Vec::new();
        self.each(sql, params, |row| {
            all.push(map(&row)?);
            Ok(true)
        })?;
        Ok(all)
    }

    /// The first row `sql` returns, turned into a `T` by `map`.
    ///
    /// # Errors
    ///
    /// When the SQL or `map` fails, or there is no row.
    pub fn query_row<T>(
        &self,
        sql: &str,
        params: &[&dyn Param],
        map: impl FnOnce(&Row) -> Result<T>,
    ) -> Result<T> {
        let mut first = None;
        self.each(sql, params, |row| {
            first = Some(row);
            Ok(false)
        })?;
        map(&first.ok_or_else(|| Error::new("the query returned no rows"))?)
    }

    /// One bound statement on Postgres, with the marks numbered `$N`.
    #[cfg(feature = "postgres")]
    fn run(&self, sql: &str, params: &[&dyn Param]) -> Result<super::postgres::Rows> {
        let Inner::Postgres(client) = self.0 else {
            let Inner::Unavailable(error) = self.0 else {
                unreachable!("run is for Postgres")
            };
            return Err(error.clone());
        };
        let values: Vec<_> = params.iter().map(|param| param.to_value().into()).collect();
        // The borrow ends with the statement: rows are buffered, so a `map`
        // that queries again does not find the client taken.
        Ok(client
            .borrow_mut()
            .query(&to_postgres_placeholders(sql), &values)?)
    }

    /// Hands each row to `visit` while it returns `true`.
    fn each(
        &self,
        sql: &str,
        params: &[&dyn Param],
        mut visit: impl FnMut(Row) -> Result<bool>,
    ) -> Result<()> {
        use rusqlite::types::ValueRef;
        #[cfg_attr(
            not(feature = "postgres"),
            allow(clippy::infallible_destructuring_match)
        )]
        let connection = match self.0 {
            Inner::Sqlite(connection) => connection,
            #[cfg(feature = "postgres")]
            _ => {
                let result = self.run(sql, params)?;
                let columns: Rc<[String]> = result.columns.into();
                for values in result.rows {
                    let values = values.into_iter().map(Value::from).collect();
                    let columns = columns.clone();
                    if !visit(Row { columns, values })? {
                        break;
                    }
                }
                return Ok(());
            }
        };
        let mut statement = connection.prepare(sql)?;
        let columns: Rc<[String]> = statement
            .column_names()
            .into_iter()
            .map(String::from)
            .collect();
        let mut rows = statement.query(bind(params))?;
        while let Some(row) = rows.next()? {
            let values = (0..columns.len())
                .map(|at| {
                    Ok(match row.get_ref(at)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => Value::Integer(n),
                        ValueRef::Real(n) => Value::Real(n),
                        ValueRef::Text(text) => Value::Text(String::from_utf8_lossy(text).into()),
                        ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
                    })
                })
                .collect::<Result<_>>()?;
            let more = visit(Row {
                columns: columns.clone(),
                values,
            })?;
            if !more {
                break;
            }
        }
        Ok(())
    }
}

/// A list of [`Param`]s for a statement, of any types:
/// `params![title, 3, None::<String>]`.
#[doc(hidden)]
#[macro_export]
macro_rules! __db_params {
    ($($value:expr),* $(,)?) => {
        &[$(&$value as &dyn $crate::db::Param),*] as &[&dyn $crate::db::Param]
    };
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::db::testing::{each_engine, is_postgres};
    use crate::db::{Db, Error, FromColumn, Value, params};

    #[test]
    fn values_round_trip_and_mismatches_name_the_column() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let id = crate::uuid::Uuid::v7();
        db.with(|sql| {
            let row = sql.query_row(
                "SELECT ?1 AS n, ?2 AS t, ?3 AS b, ?4 AS r, ?5 AS z, ?6 AS u",
                params![7, "hi", true, 1.5, None::<i64>, id],
                |row| {
                    Ok((
                        row.get::<i64>("N")?,
                        row.get::<String>(1)?,
                        row.get::<bool>("b")?,
                        row.get::<f64>("r")?,
                        row.get::<Option<i64>>("z")?,
                        row.get::<crate::uuid::Uuid>("u")?,
                    ))
                },
            )?;
            assert_eq!(row, (7, "hi".into(), true, 1.5, None, id));
            let error = sql
                .query_row("SELECT 'x' AS n", &[], |row| row.get::<i64>("n"))
                .unwrap_err();
            assert!(
                error.to_string().starts_with("column n: expected"),
                "{error}"
            );
            assert!(
                sql.query_row("SELECT 1", &[], |row| row.get::<i64>("x"))
                    .is_err()
            );
            assert!(
                sql.query_row("SELECT 1 WHERE 0", &[], |row| row.get::<i64>(0))
                    .is_err()
            );
            let blob = sql.query_row("SELECT x'00ff'", &[], |row| row.get::<Value>(0))?;
            assert_eq!(blob, Value::Blob(vec![0, 255]));
            Ok::<_, crate::db::Error>(())
        })
        .unwrap();
    }

    #[test]
    fn bools_bind_as_integers_and_read_back_from_either() {
        use crate::db::Param;
        assert_eq!(true.to_value(), Value::Bool(true));
        assert_eq!(Option::<bool>::None.to_value(), Value::Null);
        assert!(bool::from_value(Value::Bool(true)).unwrap());
        assert!(!bool::from_value(Value::Integer(0)).unwrap());
        assert!(bool::from_value(Value::Integer(2)).unwrap());
        assert!(bool::from_value(Value::Text("t".into())).is_err());
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let stored: i64 = db
            .with(|sql| sql.query_row("SELECT ?1", params![true], |row| row.get(0)))
            .unwrap();
        assert_eq!(stored, 1);
    }

    #[test]
    fn unique_violations_are_told_apart_on_both_engines() {
        each_engine(|db| {
            db.with(|sql| {
                sql.execute_batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER UNIQUE, m INTEGER NOT NULL)",
                )?;
                sql.execute("INSERT INTO t VALUES (1, 1, 1)", &[])
            })
            .unwrap();
            let fail = |sql: &str| db.with(|c| c.execute(sql, &[])).unwrap_err();
            let unique = fail("INSERT INTO t VALUES (2, 1, 1)");
            assert!(unique.is_unique_violation(), "{unique}");
            assert!(fail("INSERT INTO t VALUES (1, 2, 1)").is_unique_violation());
            let null = fail("INSERT INTO t VALUES (3, 3, NULL)");
            assert!(!null.is_unique_violation(), "{null}");
            assert!(!fail("NOT SQL").is_unique_violation());
            let postgres = is_postgres(db);
            assert_eq!(unique.sqlstate(), postgres.then_some("23505"));
            assert_eq!(null.sqlstate(), postgres.then_some("23502"));
        });
        let pg = Error {
            message: "duplicate key".into(),
            sqlstate: Some("23505".into()),
            sqlite_code: None,
        };
        assert!(pg.is_unique_violation());
        assert_eq!(pg.sqlstate(), Some("23505"));
        let other = Error {
            sqlstate: Some("23503".into()),
            ..pg
        };
        assert!(!other.is_unique_violation());
        assert!(!Error::new("local").is_unique_violation());
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn values_round_trip_on_postgres() {
        let Some(db) = crate::db::testing::postgres() else {
            return;
        };
        let id = crate::uuid::Uuid::v7();
        db.with(|sql| {
            // Parameters are sent untyped, so the server needs casts to type them.
            let row = sql.query_row(
                "SELECT ?1::bigint AS n, ?2::text AS t, ?3::boolean AS b, ?4::double precision AS r, ?5::bigint AS z, ?6::text AS u, ?7::bytea AS x",
                params![7, "hi", true, 1.5, None::<i64>, id, vec![0u8, 255]],
                |row| {
                    Ok((
                        row.get::<i64>("N")?,
                        row.get::<String>(1)?,
                        row.get::<bool>("b")?,
                        row.get::<f64>("r")?,
                        row.get::<Option<i64>>("z")?,
                        row.get::<crate::uuid::Uuid>("u")?,
                        row.get::<Value>("x")?,
                    ))
                },
            )?;
            assert_eq!(row, (7, "hi".into(), true, 1.5, None, id, Value::Blob(vec![0, 255])));
            let error = sql
                .query_row("SELECT 'x' AS n", &[], |row| row.get::<i64>("n"))
                .unwrap_err();
            assert!(error.to_string().starts_with("column n: expected"), "{error}");
            // Marks in literals and comments are not parameters.
            let marks = sql.query_row("SELECT '?' || ?1::text -- ?", params!["x"], |row| row.get::<String>(0))?;
            assert_eq!(marks, "?x");
            Ok::<_, Error>(())
        })
        .unwrap();
    }
}
