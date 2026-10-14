use std::marker::PhantomData;

use crate::{
    Db, Error, FromColumn, Id, Record, Result, Timestamp, ToValue, Value, rel::Append, rel::RelSet, rel::Relation,
};

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

    /// The column's value is among the subquery's rows: the typed way to filter through
    /// another table (a semi-join). The subquery must yield this column's value type.
    ///
    /// ```ignore
    /// // Repositories in the `afomera` namespace (a `Query<U>` yields its ids):
    /// Repository::query().filter(repository::namespace_id.in_query(Namespace::query().filter(namespace::path.eq("afomera"))))
    /// // Repositories `me` collaborates on (`.select` picks another column):
    /// Repository::query().filter(repository::id.in_query(
    ///     Collaborator::query().filter(collaborator::user_id.eq(me)).select(collaborator::repository_id),
    /// ))
    /// ```
    pub fn in_query(self, subquery: impl Subquery<V>) -> Expr<T> {
        let (sql, binds) = subquery.into_sql();
        Expr::new(format!("\"{}\" IN ({sql})", self.name), binds)
    }

    /// The column's value is not among the subquery's rows. Rows where this column is NULL
    /// never match (SQL's NOT IN); NULLs the subquery yields are skipped, so they don't
    /// empty the result.
    pub fn not_in_query(self, subquery: impl Subquery<V>) -> Expr<T> {
        let (sql, binds) = subquery.into_sql();
        Expr::new(format!("\"{}\" NOT IN ({sql})", self.name), binds)
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

    /// Kiln's own SQL (sign-in lookups); not for apps.
    pub(crate) fn raw(sql: &str, binds: Vec<Value>) -> Self {
        Self::new(sql.to_owned(), binds)
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
    lock: bool,
    _rels: PhantomData<fn() -> R>,
}

// By hand: derives would require `T: Clone` and `R: Clone`, which the markers don't need.
impl<T> Clone for Expr<T> {
    fn clone(&self) -> Self {
        Self::new(self.sql.clone(), self.binds.clone())
    }
}

impl<T> Clone for Order<T> {
    fn clone(&self) -> Self {
        Self::new(self.sql.clone())
    }
}

/// A base query can be reused, e.g. for several aggregates over the same rows.
impl<T, R> Clone for Query<T, R> {
    fn clone(&self) -> Self {
        Self {
            filters: self.filters.clone(),
            orders: self.orders.clone(),
            limit: self.limit,
            offset: self.offset,
            lock: self.lock,
            _rels: PhantomData,
        }
    }
}

impl<T: Record, R> Default for Query<T, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Record, R> Query<T, R> {
    pub fn new() -> Self {
        Self { filters: vec![], orders: vec![], limit: None, offset: None, lock: false, _rels: PhantomData }
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
        self.with_rels()
    }

    fn with_rels<N>(self) -> Query<T, N> {
        Query {
            filters: self.filters,
            orders: self.orders,
            limit: self.limit,
            offset: self.offset,
            lock: self.lock,
            _rels: PhantomData,
        }
    }

    /// Locks the rows `all`/`one`/`one_or_404` read until the transaction ends, so a
    /// read-then-write (handing out the next issue number) can't race. Only inside
    /// [`Db::transaction`]; preloaded relations aren't locked.
    ///
    /// Postgres: `SELECT .. FOR UPDATE`. SQLite has no row locks; it takes the database's
    /// write lock instead (a no-op write first), which other writers wait on just the same.
    /// SQLite's transactions begin deferred, so lock before reading anything else in the
    /// transaction, or SQLite reports the database busy rather than waiting.
    ///
    /// ```ignore
    /// ctx.db.transaction(|tx| async move {
    ///     let repo = Repository::find(id).for_update().one_or_404(&tx).await?;
    ///     Repository::update(id).set(repository::issue_count, repo.issue_count + 1).run(&tx).await
    /// }).await?;
    /// ```
    pub fn for_update(mut self) -> Self {
        self.lock = true;
        self
    }

    /// One column of the matching rows: run it with [`Select::all`], or pass it to
    /// [`Column::in_query`] as a subquery.
    pub fn select<V>(self, column: Column<T, V>) -> Select<T, V> {
        Select { query: self.with_rels(), column: column.name(), _marker: PhantomData }
    }

    fn where_sql(&self, sql: &mut String, binds: &mut Vec<Value>) {
        if self.filters.is_empty() {
            return;
        }
        let clauses: Vec<String> = self.filters.iter().map(|f| format!("({})", f.sql)).collect();
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
        for f in &self.filters {
            binds.extend(f.binds.iter().cloned());
        }
    }

    fn to_sql(&self, select: &str, ordered: bool) -> (String, Vec<Value>) {
        let mut sql = format!("SELECT {select} FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        self.where_sql(&mut sql, &mut binds);
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

    /// Matching rows counted per value of `column`, in the column's order (NULL first):
    /// `Reaction::query().filter(..).count_by(reaction::emoji, &db)` gives `[("+1", 3), ..]`.
    /// Order, limit and offset are ignored.
    pub async fn count_by<V: FromColumn>(self, column: Column<T, V>, db: &Db) -> Result<Vec<(V, i64)>> {
        let name = column.name();
        let mut sql = format!("SELECT \"{name}\" AS \"key\", COUNT(*) AS \"count\" FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        self.where_sql(&mut sql, &mut binds);
        // SQLite sorts NULL first and Postgres last; pin it so results match.
        sql.push_str(&format!(" GROUP BY \"{name}\" ORDER BY \"{name}\" NULLS FIRST"));
        let rows = db.fetch_all(&sql, binds).await?;
        let pairs = rows.iter().map(|row| Ok((V::from_column(row, "key")?, i64::from_column(row, "count")?)));
        pairs.collect::<std::result::Result<_, sqlx::Error>>().map_err(Error::from)
    }

    /// Sum of `column` over the matching rows; 0 when none match (NULLs are skipped).
    pub async fn sum<V: Summable>(self, column: Column<T, V>, db: &Db) -> Result<V::Total> {
        // Postgres sums integers as NUMERIC, which sqlx's `Any` driver can't read: cast back.
        let select = format!("CAST(COALESCE(SUM(\"{}\"), 0) AS {}) AS \"total\"", column.name(), V::CAST);
        self.aggregate(&select, db).await
    }

    /// Largest value of `column` among the matching rows; `None` when none match.
    pub async fn max<V: MinMax>(self, column: Column<T, V>, db: &Db) -> Result<Option<V::Value>> {
        self.aggregate(&format!("MAX(\"{}\") AS \"total\"", column.name()), db).await
    }

    /// Smallest value of `column` among the matching rows; `None` when none match.
    pub async fn min<V: MinMax>(self, column: Column<T, V>, db: &Db) -> Result<Option<V::Value>> {
        self.aggregate(&format!("MIN(\"{}\") AS \"total\"", column.name()), db).await
    }

    async fn aggregate<V: FromColumn>(self, select: &str, db: &Db) -> Result<V> {
        let mut sql = format!("SELECT {select} FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        self.where_sql(&mut sql, &mut binds);
        let rows = db.fetch_all(&sql, binds).await?;
        Ok(V::from_column(rows.first().ok_or(Error::NotFound)?, "total")?)
    }

    /// Deletes every matching row and returns how many. Order and limit are ignored.
    pub async fn delete_all(self, db: &Db) -> Result<u64> {
        let mut sql = format!("DELETE FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        self.where_sql(&mut sql, &mut binds);
        sql.push_str(" RETURNING \"id\"");
        Ok(db.fetch_all(&sql, binds).await?.len() as u64)
    }
}

impl<T: Record, R: RelSet<T>> Query<T, R> {
    /// Runs the query plus one query per preloaded relation (never one per row).
    pub async fn all(self, db: &Db) -> Result<Vec<R::Row>> {
        let select = quoted_columns::<T>();
        let (mut sql, binds) = self.to_sql(&select, true);
        if self.lock {
            lock_rows::<T>(db, &mut sql).await?;
        }
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

/// Prepares a `for_update` read: refuses outside a transaction, where the lock would be
/// released as soon as it's taken, and adds the backend's locking.
async fn lock_rows<T: Record>(db: &Db, sql: &mut String) -> Result<()> {
    if !db.in_transaction() {
        return Err(Error::Config(format!(
            "`for_update()` on `{}` ran outside a transaction, so its lock would end with the query; run it inside `db.transaction(|tx| ..)` and pass `&tx`",
            T::TABLE
        )));
    }
    if let Some(write_lock) = db.dialect().write_lock(T::TABLE) {
        db.fetch_all(&write_lock, vec![]).await?;
    }
    sql.push_str(db.dialect().row_lock());
    Ok(())
}

/// One column of a query's rows, from [`Query::select`].
pub struct Select<T, V> {
    query: Query<T>,
    column: &'static str,
    _marker: PhantomData<fn() -> V>,
}

impl<T: Record, V: FromColumn> Select<T, V> {
    /// The column's values, in the query's order (`pluck`).
    pub async fn all(self, db: &Db) -> Result<Vec<V>> {
        let (sql, binds) = self.query.to_sql(&format!("\"{}\"", self.column), true);
        let rows = db.fetch_all(&sql, binds).await?;
        rows.iter().map(|row| V::from_column(row, self.column).map_err(Error::from)).collect()
    }
}

impl<U: Record, V> Select<U, V> {
    fn subquery_sql(self) -> (String, Vec<Value>) {
        let query = self.query.filter(Expr::new(format!("\"{}\" IS NOT NULL", self.column), vec![]));
        // Order only matters when it picks which rows a limit keeps.
        let ordered = query.limit.is_some() || query.offset.is_some();
        query.to_sql(&format!("\"{}\"", self.column), ordered)
    }
}

/// A subquery yielding values of type `V`, for [`Column::in_query`]: a
/// `Query<U>::select(column)` of that type, or a `Query<U>` itself, which yields its ids.
/// `Option<V>` and `V` mix either way: the subquery skips NULLs.
pub trait Subquery<V> {
    /// The `SELECT` and its binds.
    fn into_sql(self) -> (String, Vec<Value>);
}

impl<U: Record, V> Subquery<V> for Select<U, V> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, V> Subquery<Option<V>> for Select<U, V> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, V> Subquery<V> for Select<U, Option<V>> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, R> Subquery<Id<U>> for Query<U, R> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.select(Column::<U, Id<U>>::new("id")).subquery_sql()
    }
}

impl<U: Record, R> Subquery<Option<Id<U>>> for Query<U, R> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.select(Column::<U, Id<U>>::new("id")).subquery_sql()
    }
}

/// A column type [`Query::sum`] adds up: integers to `i64`, reals to `f64`.
pub trait Summable {
    type Total: FromColumn;
    /// The SQL type the sum is cast to, so every backend returns the same type.
    const CAST: &'static str;
}

impl Summable for i64 {
    type Total = i64;
    const CAST: &'static str = "BIGINT";
}

impl Summable for i32 {
    type Total = i64;
    const CAST: &'static str = "BIGINT";
}

impl Summable for f64 {
    type Total = f64;
    const CAST: &'static str = "DOUBLE PRECISION";
}

impl<V: Summable> Summable for Option<V> {
    type Total = V::Total;
    const CAST: &'static str = V::CAST;
}

/// A column type [`Query::max`] and [`Query::min`] work on. `Option` columns give the
/// inner type: the result is `None` only when no row has a value.
pub trait MinMax {
    type Value: FromColumn;
}

macro_rules! min_max {
    ($($t:ty),*) => {$(
        impl MinMax for $t {
            type Value = $t;
        }
    )*};
}

min_max!(i64, i32, f64, String, Timestamp);

impl<U> MinMax for Id<U> {
    type Value = Id<U>;
}

impl<V: MinMax> MinMax for Option<V> {
    type Value = V::Value;
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
    increments: Vec<(&'static str, i64)>,
}

impl<T: Record> Update<T> {
    pub(crate) fn new(id: Id<T>) -> Self {
        Self { id, sets: vec![], increments: vec![] }
    }

    /// Adds `by` (negative to subtract) to the stored value in the same statement, so
    /// concurrent increments never lose one; NULL counts as 0. The returned record has the
    /// new value, e.g. the next per-repository issue number:
    /// `Repository::update(id).increment(repository::issue_count, 1).run(&tx).await?.issue_count`.
    pub fn increment<V: Summable<Total = i64>>(mut self, column: Column<T, V>, by: i64) -> Self {
        self.increments.push((column.name(), by));
        self
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
        let mut assignments: Vec<String> = self.sets.iter().map(|(n, _)| format!("\"{n}\" = ?")).collect();
        assignments.extend(self.increments.iter().map(|(n, _)| format!("\"{n}\" = COALESCE(\"{n}\", 0) + ?")));
        let sql = format!(
            "UPDATE \"{}\" SET {} WHERE \"id\" = ? RETURNING {}",
            T::TABLE,
            assignments.join(", "),
            quoted_columns::<T>()
        );
        let mut binds: Vec<Value> = self.sets.into_iter().map(|(_, v)| v).collect();
        binds.extend(self.increments.into_iter().map(|(_, by)| Value::Integer(by)));
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
