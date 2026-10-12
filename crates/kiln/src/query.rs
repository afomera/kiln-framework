use std::marker::PhantomData;

use crate::{Db, Error, Id, Record, Result, Timestamp, ToValue, Value, rel::Append, rel::RelSet, rel::Relation};

/// A typed column of record `T` holding values of type `V`.
/// `#[derive(Record)]` generates one constant per field, e.g. `post::slug`.
pub struct Column<T, V> {
    name: &'static str,
    _marker: PhantomData<fn() -> (T, V)>,
}

impl<T, V> Clone for Column<T, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, V> Copy for Column<T, V> {}

impl<T, V> Column<T, V> {
    pub const fn new(name: &'static str) -> Self {
        Self { name, _marker: PhantomData }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub fn asc(self) -> Order<T> {
        Order::new(format!("\"{}\" ASC", self.name))
    }

    pub fn desc(self) -> Order<T> {
        Order::new(format!("\"{}\" DESC", self.name))
    }

    fn compare(self, op: &str, value: Value) -> Expr<T> {
        Expr::new(format!("\"{}\" {op} ?", self.name), vec![value])
    }
}

impl<T, V: ToValue> Column<T, V> {
    pub fn eq(self, value: impl Into<V>) -> Expr<T> {
        self.compare("=", value.into().to_value())
    }

    pub fn ne(self, value: impl Into<V>) -> Expr<T> {
        self.compare("<>", value.into().to_value())
    }

    pub fn lt(self, value: impl Into<V>) -> Expr<T> {
        self.compare("<", value.into().to_value())
    }

    pub fn lte(self, value: impl Into<V>) -> Expr<T> {
        self.compare("<=", value.into().to_value())
    }

    pub fn gt(self, value: impl Into<V>) -> Expr<T> {
        self.compare(">", value.into().to_value())
    }

    pub fn gte(self, value: impl Into<V>) -> Expr<T> {
        self.compare(">=", value.into().to_value())
    }

    pub fn is_in<I>(self, values: I) -> Expr<T>
    where
        I: IntoIterator,
        I::Item: Into<V>,
    {
        let binds: Vec<Value> = values.into_iter().map(|v| v.into().to_value()).collect();
        if binds.is_empty() {
            return Expr::new("0 = 1".to_owned(), vec![]);
        }
        let marks = vec!["?"; binds.len()].join(", ");
        Expr::new(format!("\"{}\" IN ({marks})", self.name), binds)
    }
}

impl<T, V> Column<T, Option<V>> {
    pub fn is_null(self) -> Expr<T> {
        Expr::new(format!("\"{}\" IS NULL", self.name), vec![])
    }

    pub fn is_not_null(self) -> Expr<T> {
        Expr::new(format!("\"{}\" IS NOT NULL", self.name), vec![])
    }
}

impl<T> Column<T, String> {
    pub fn like(self, pattern: &str) -> Expr<T> {
        self.compare("LIKE", Value::Text(pattern.to_owned()))
    }
}

/// A boolean SQL expression over record `T`. Filtering a `Query<Post>` with a
/// `Comment` column is a type error.
pub struct Expr<T> {
    sql: String,
    binds: Vec<Value>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Expr<T> {
    fn new(sql: String, binds: Vec<Value>) -> Self {
        Self { sql, binds, _marker: PhantomData }
    }

    pub fn and(self, other: Expr<T>) -> Expr<T> {
        self.join("AND", other)
    }

    pub fn or(self, other: Expr<T>) -> Expr<T> {
        self.join("OR", other)
    }

    fn join(mut self, op: &str, other: Expr<T>) -> Expr<T> {
        self.binds.extend(other.binds);
        Expr::new(format!("({}) {op} ({})", self.sql, other.sql), self.binds)
    }
}

pub struct Order<T> {
    sql: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Order<T> {
    fn new(sql: String) -> Self {
        Self { sql, _marker: PhantomData }
    }
}

/// A query for records of type `T` with preloaded relations `R`.
///
/// `R` is a tuple of relation markers (`(post::author, post::comments)`). It only changes
/// through [`Query::preload`], and it decides the row type: plain `T` when nothing is
/// preloaded, `Loaded<T, R>` otherwise.
pub struct Query<T, R = ()> {
    filters: Vec<Expr<T>>,
    orders: Vec<Order<T>>,
    limit: Option<u64>,
    offset: Option<u64>,
    _rels: PhantomData<fn() -> R>,
}

impl<T: Record, R> Default for Query<T, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Record, R> Query<T, R> {
    pub fn new() -> Self {
        Self { filters: vec![], orders: vec![], limit: None, offset: None, _rels: PhantomData }
    }

    pub fn filter(mut self, expr: Expr<T>) -> Self {
        self.filters.push(expr);
        self
    }

    pub fn order(mut self, order: Order<T>) -> Self {
        self.orders.push(order);
        self
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// One-based page of `per` rows.
    pub fn page(self, page: u64, per: u64) -> Self {
        self.limit(per).offset(page.saturating_sub(1) * per)
    }

    /// Adds a relation to load alongside the records, in one extra query per relation.
    pub fn preload<X>(self, _relation: X) -> Query<T, R::Output>
    where
        X: Relation<T>,
        R: Append<X>,
    {
        Query { filters: self.filters, orders: self.orders, limit: self.limit, offset: self.offset, _rels: PhantomData }
    }

    fn to_sql(&self, select: &str, ordered: bool) -> (String, Vec<Value>) {
        let mut sql = format!("SELECT {select} FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        if !self.filters.is_empty() {
            let clauses: Vec<String> = self.filters.iter().map(|f| format!("({})", f.sql)).collect();
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
            for f in &self.filters {
                binds.extend(f.binds.iter().cloned());
            }
        }
        if ordered {
            sql.push_str(" ORDER BY ");
            if self.orders.is_empty() {
                sql.push_str("\"id\" ASC");
            } else {
                let orders: Vec<&str> = self.orders.iter().map(|o| o.sql.as_str()).collect();
                sql.push_str(&orders.join(", "));
            }
        }
        match (self.limit, self.offset) {
            (Some(l), Some(o)) => sql.push_str(&format!(" LIMIT {l} OFFSET {o}")),
            (Some(l), None) => sql.push_str(&format!(" LIMIT {l}")),
            (None, Some(o)) => sql.push_str(&format!(" LIMIT -1 OFFSET {o}")),
            (None, None) => {}
        }
        (sql, binds)
    }

    pub async fn count(self, db: &Db) -> Result<i64> {
        let (sql, binds) = self.to_sql("COUNT(*)", false);
        let rows = db.fetch_all(&sql, binds).await?;
        let row = rows.first().ok_or(Error::NotFound)?;
        Ok(sqlx::Row::try_get(row, 0)?)
    }

    pub async fn exists(self, db: &Db) -> Result<bool> {
        Ok(self.limit(1).count(db).await? > 0)
    }

    /// Deletes every matching row and returns how many. Order and limit are ignored.
    pub async fn delete_all(self, db: &Db) -> Result<u64> {
        let mut sql = format!("DELETE FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        if !self.filters.is_empty() {
            let clauses: Vec<String> = self.filters.iter().map(|f| format!("({})", f.sql)).collect();
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
            for f in self.filters {
                binds.extend(f.binds);
            }
        }
        sql.push_str(" RETURNING \"id\"");
        Ok(db.fetch_all(&sql, binds).await?.len() as u64)
    }
}

impl<T: Record, R: RelSet<T>> Query<T, R> {
    /// Runs the query plus one query per preloaded relation (never one per row).
    pub async fn all(self, db: &Db) -> Result<Vec<R::Row>> {
        let select = quoted_columns::<T>();
        let (sql, binds) = self.to_sql(&select, true);
        let rows = db.fetch_all(&sql, binds).await?;
        let records = rows.iter().map(T::from_row).collect::<std::result::Result<Vec<T>, _>>()?;
        let data = R::load(&records, db).await?;
        if data.len() != records.len() {
            return Err(Error::Integrity(format!(
                "relation loader for `{}` returned {} rows for {} records",
                T::TABLE,
                data.len(),
                records.len()
            )));
        }
        Ok(records.into_iter().zip(data).map(|(record, data)| R::assemble(record, data)).collect())
    }

    pub async fn one(self, db: &Db) -> Result<Option<R::Row>> {
        Ok(self.limit(1).all(db).await?.into_iter().next())
    }

    /// Like [`Query::one`], but a missing row becomes `Error::NotFound` (HTTP 404).
    pub async fn one_or_404(self, db: &Db) -> Result<R::Row> {
        self.one(db).await?.ok_or(Error::MissingRecord { table: T::TABLE })
    }
}

fn quoted_columns<T: Record>() -> String {
    T::COLUMNS.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")
}

pub struct Insert<T> {
    values: Vec<(&'static str, Value)>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Record> Insert<T> {
    pub(crate) fn new(values: Vec<(&'static str, Value)>) -> Self {
        Self { values, _marker: PhantomData }
    }

    /// Inserts the row and returns the stored record.
    pub async fn run(self, db: &Db) -> Result<T> {
        let names: Vec<String> = self.values.iter().map(|(n, _)| format!("\"{n}\"")).collect();
        let marks = vec!["?"; self.values.len()].join(", ");
        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({marks}) RETURNING {}",
            T::TABLE,
            names.join(", "),
            quoted_columns::<T>()
        );
        let binds = self.values.into_iter().map(|(_, v)| v).collect();
        let rows = db.fetch_all(&sql, binds).await?;
        Ok(T::from_row(rows.first().ok_or(Error::NotFound)?)?)
    }
}

pub struct Update<T> {
    id: Id<T>,
    sets: Vec<(&'static str, Value)>,
}

impl<T: Record> Update<T> {
    pub(crate) fn new(id: Id<T>) -> Self {
        Self { id, sets: vec![] }
    }

    pub fn set<V: ToValue>(mut self, column: Column<T, V>, value: impl Into<V>) -> Self {
        self.sets.push((column.name(), value.into().to_value()));
        self
    }

    /// Applies the update and returns the stored record. `updated_at` is set automatically.
    pub async fn run(mut self, db: &Db) -> Result<T> {
        if T::HAS_UPDATED_AT && !self.sets.iter().any(|(n, _)| *n == "updated_at") {
            self.sets.push(("updated_at", Timestamp::now().to_value()));
        }
        let assignments: Vec<String> = self.sets.iter().map(|(n, _)| format!("\"{n}\" = ?")).collect();
        let sql = format!(
            "UPDATE \"{}\" SET {} WHERE \"id\" = ? RETURNING {}",
            T::TABLE,
            assignments.join(", "),
            quoted_columns::<T>()
        );
        let mut binds: Vec<Value> = self.sets.into_iter().map(|(_, v)| v).collect();
        binds.push(self.id.to_value());
        let rows = db.fetch_all(&sql, binds).await?;
        Ok(T::from_row(rows.first().ok_or(Error::NotFound)?)?)
    }
}

pub struct Delete<T> {
    id: Id<T>,
}

impl<T: Record> Delete<T> {
    pub(crate) fn new(id: Id<T>) -> Self {
        Self { id }
    }

    /// Deletes the row. A missing row is `Error::MissingRecord` (404).
    pub async fn run(self, db: &Db) -> Result<()> {
        let sql = format!("DELETE FROM \"{}\" WHERE \"id\" = ? RETURNING \"id\"", T::TABLE);
        let rows = db.fetch_all(&sql, vec![self.id.to_value()]).await?;
        if rows.is_empty() {
            return Err(Error::MissingRecord { table: T::TABLE });
        }
        Ok(())
    }
}
