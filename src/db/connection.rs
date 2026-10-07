//! The database types an app sees: [`Connection`], [`Row`], [`Value`],
//! [`Param`], [`FromColumn`] and [`Error`]. They name no engine, so the same
//! app code can run on another one (ADR 0034).

use std::rc::Rc;

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
}

/// A database error: the engine's message, or why a column could not be read
/// as the type asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error with `message`, for a [`FromColumn`] or [`Model`](super::Model)
    /// that rejects what it read.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
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
                    .map_err(|_| Error(format!("{n} does not fit in {}", stringify!($type))))
            }
        }
    )*};
}

small_integers!(i32, u32);

impl Param for bool {
    fn to_value(&self) -> Value {
        Value::Integer((*self).into())
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
    Err(Error(format!("expected {wanted}, found {value:?}")))
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

/// `0` is false, any other integer true.
impl FromColumn for bool {
    fn from_value(value: Value) -> Result<Self> {
        i64::from_value(value).map(|n| n != 0)
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
        Self::parse(&text).ok_or_else(|| Error(format!("not a UUID: {text}")))
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
            return Err(Error(format!("no such column in {:?}", self.columns)));
        };
        T::from_value(self.values[at].clone())
            .map_err(|Error(problem)| Error(format!("column {}: {problem}", self.columns[at])))
    }
}

/// The connection [`Db::with`](super::Db::with), [`Db::read`](super::Db::read)
/// and a [`Tx`](super::Tx) lend. SQL marks are `?` (in order) or `?N` (the
/// `N`th value, from 1).
#[derive(Clone, Copy)]
// ponytail: SQLite only; step 2 of ADR 0034 makes this an enum with a
// Postgres arm, rewriting `?`/`?N` to `$N`.
pub struct Connection<'a>(pub(super) &'a rusqlite::Connection);

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
        }))
    }
}

fn bind(params: &[&dyn Param]) -> impl rusqlite::Params {
    rusqlite::params_from_iter(params.iter().map(|param| param.to_value()))
}

impl Connection<'_> {
    /// Runs one statement and returns how many rows it changed.
    ///
    /// # Errors
    ///
    /// When the SQL fails.
    pub fn execute(&self, sql: &str, params: &[&dyn Param]) -> Result<usize> {
        Ok(self.0.execute(sql, bind(params))?)
    }

    /// Runs any number of `;`-separated statements, without parameters.
    ///
    /// # Errors
    ///
    /// When any of them fails; the ones before it stay done.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        Ok(self.0.execute_batch(sql)?)
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
        map(&first.ok_or_else(|| Error("the query returned no rows".into()))?)
    }

    /// Hands each row to `visit` while it returns `true`.
    fn each(
        &self,
        sql: &str,
        params: &[&dyn Param],
        mut visit: impl FnMut(Row) -> Result<bool>,
    ) -> Result<()> {
        use rusqlite::types::ValueRef;
        let mut statement = self.0.prepare(sql)?;
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
    use crate::db::{Db, Value, params};

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
}
