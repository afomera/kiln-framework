//! Raw SQL: the escape hatch for what `Record::query()` can't say (joins that order by
//! another table, window functions, reports). Prefer the typed query builder; reach for
//! this when it runs out.

use sqlx::{Column as _, Row as _, ValueRef as _, any::AnyTypeInfoKind};

use crate::{Db, Error, FromColumn, Record, Result, Row, ToValue, Value};

/// A raw SQL statement with `?` binds, from [`Db::sql`]. Placeholders are translated for the
/// backend, and it runs on whatever the handle is bound to, a transaction included.
///
/// ```ignore
/// let repos: Vec<Repository> = db
///     .sql("SELECT r.* FROM \"repositories\" r JOIN \"namespaces\" n ON n.\"id\" = r.\"namespace_id\" WHERE n.\"path\" = ? ORDER BY n.\"path\", r.\"name\"")
///     .bind("afomera")
///     .records()
///     .await?;
/// let counts: Vec<(String, i64)> = db.sql("SELECT \"emoji\", COUNT(*) FROM \"reactions\" GROUP BY \"emoji\"").all().await?;
/// ```
#[must_use = "a statement does nothing until it's run with `.all()`, `.records()`, `.run()`, .."]
pub struct Sql<'a> {
    db: &'a Db,
    sql: String,
    binds: Vec<Value>,
}

impl Db {
    /// A raw SQL statement: the escape hatch when `Record::query()` can't express a query.
    /// Write it once for every backend: `?` placeholders (filled by `.bind(..)` in order),
    /// double-quoted identifiers, no backend-only functions.
    pub fn sql(&self, sql: impl Into<String>) -> Sql<'_> {
        Sql { db: self, sql: sql.into(), binds: vec![] }
    }
}

impl Sql<'_> {
    /// Fills the next `?`.
    pub fn bind(mut self, value: impl ToValue) -> Self {
        self.binds.push(value.to_value());
        self
    }

    /// Rows decoded as records of `T`. The statement must return every one of `T`'s
    /// columns (`SELECT "repositories".* ..`).
    pub async fn records<T: Record>(self) -> Result<Vec<T>> {
        let rows = self.db.fetch_all(&self.sql, self.binds).await?;
        rows.iter().map(|row| T::from_row(row).map_err(Error::from)).collect()
    }

    /// Rows decoded by position: a value (`i64`, `String`, `Id<T>`, ..) for one column, a
    /// tuple for several.
    pub async fn all<R: FromSqlRow>(self) -> Result<Vec<R>> {
        let rows = self.db.fetch_all(&self.sql, self.binds).await?;
        rows.iter().map(R::from_sql_row).collect()
    }

    /// The first row, decoded like [`Sql::all`]; `None` when there are no rows.
    pub async fn one<R: FromSqlRow>(self) -> Result<Option<R>> {
        let rows = self.db.fetch_all(&self.sql, self.binds).await?;
        rows.first().map(R::from_sql_row).transpose()
    }

    /// Rows as JSON objects keyed by column name: integers, reals, text and NULL. Postgres
    /// `NUMERIC` (decimal literals, `SUM`, `AVG`) has no portable reading: `CAST(.. AS DOUBLE PRECISION)`.
    pub async fn json(self) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        let rows = self.db.fetch_all(&self.sql, self.binds).await?;
        rows.iter().map(row_json).collect()
    }

    /// The rows as returned by sqlx, to decode by hand.
    pub async fn rows(self) -> Result<Vec<Row>> {
        self.db.fetch_all(&self.sql, self.binds).await
    }

    /// Runs a statement that returns no rows (`UPDATE`, `DELETE`, ..); gives the rows affected.
    pub async fn run(self) -> Result<u64> {
        self.db.execute_with(&self.sql, self.binds).await
    }
}

/// A row shape [`Sql::all`] decodes: one column's value, or a tuple of them.
pub trait FromSqlRow: Sized {
    fn from_sql_row(row: &Row) -> Result<Self>;
}

/// The name of column `index`, to decode it with `FromColumn`.
fn column_name(row: &Row, index: usize, arity: usize) -> Result<&str> {
    match row.columns().get(index) {
        Some(column) => Ok(column.name()),
        None => Err(Error::Config(format!(
            "the SQL returned {} column(s) but the row type reads {arity}; select one column per tuple element",
            row.columns().len()
        ))),
    }
}

impl<V: FromColumn> FromSqlRow for V {
    fn from_sql_row(row: &Row) -> Result<Self> {
        Ok(V::from_column(row, column_name(row, 0, 1)?)?)
    }
}

macro_rules! tuple_from_sql_row {
    ($n:literal: $($A:ident $i:tt),+) => {
        impl<$($A: FromColumn),+> FromSqlRow for ($($A,)+) {
            fn from_sql_row(row: &Row) -> Result<Self> {
                Ok(($($A::from_column(row, column_name(row, $i, $n)?)?,)+))
            }
        }
    };
}

tuple_from_sql_row!(1: A 0);
tuple_from_sql_row!(2: A 0, B 1);
tuple_from_sql_row!(3: A 0, B 1, C 2);
tuple_from_sql_row!(4: A 0, B 1, C 2, D 3);
tuple_from_sql_row!(5: A 0, B 1, C 2, D 3, E 4);
tuple_from_sql_row!(6: A 0, B 1, C 2, D 3, E 4, F 5);
tuple_from_sql_row!(7: A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple_from_sql_row!(8: A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);

fn row_json(row: &Row) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut map = serde_json::Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let raw = row.try_get_raw(index)?;
        // The value's own type: SQLite expression columns have no declared one.
        let value = if raw.is_null() {
            serde_json::Value::Null
        } else {
            match raw.type_info().kind() {
                AnyTypeInfoKind::Null => serde_json::Value::Null,
                AnyTypeInfoKind::Bool => row.try_get::<bool, _>(index)?.into(),
                AnyTypeInfoKind::SmallInt | AnyTypeInfoKind::Integer | AnyTypeInfoKind::BigInt => {
                    row.try_get::<i64, _>(index)?.into()
                }
                AnyTypeInfoKind::Real | AnyTypeInfoKind::Double => row.try_get::<f64, _>(index)?.into(),
                AnyTypeInfoKind::Text => row.try_get::<String, _>(index)?.into(),
                AnyTypeInfoKind::Blob => {
                    return Err(Error::Config(format!(
                        "column `{}` is binary, which JSON can't hold; decode it with `.rows()` instead",
                        column.name()
                    )));
                }
            }
        };
        map.insert(column.name().to_owned(), value);
    }
    Ok(map)
}
