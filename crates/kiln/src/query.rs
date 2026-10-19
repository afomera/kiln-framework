use std::marker::PhantomData;

use crate::{
    Db, Error, FromColumn, Id, Record, Result, Timestamp, ToValue, Value,
    join::{self, AddJoin, InScope, Inner, Join, Left, Selectable},
    rel::Append,
    rel::RelSet,
    rel::Relation,
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
}

impl<T: Record, V> Column<T, V> {
    /// `"table"."column"`. Always qualified, so a joined table's `id` or `created_at`
    /// can't make a name ambiguous.
    fn sql(&self) -> String {
        format!("\"{}\".\"{}\"", T::TABLE, self.name)
    }

    pub fn asc(self) -> Order<T> {
        Order::new(format!("{} ASC", self.sql()))
    }

    pub fn desc(self) -> Order<T> {
        Order::new(format!("{} DESC", self.sql()))
    }

    fn compare(self, op: &str, value: Value) -> Expr<T> {
        Expr::new(format!("{} {op} ?", self.sql()), vec![value])
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
        Expr::new(format!("{} IN ({sql})", self.sql()), binds)
    }

    /// The column's value is not among the subquery's rows. Rows where this column is NULL
    /// never match (SQL's NOT IN); NULLs the subquery yields are skipped, so they don't
    /// empty the result.
    pub fn not_in_query(self, subquery: impl Subquery<V>) -> Expr<T> {
        let (sql, binds) = subquery.into_sql();
        Expr::new(format!("{} NOT IN ({sql})", self.sql()), binds)
    }
}

impl<T: Record, V: ToValue> Column<T, V> {
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

    /// The column's value is one of `values`: `issue::state.is_in([State::Open, State::Draft])`,
    /// `repository::id.is_in(&ids)`. Takes values or references to them, of the column's type
    /// (`&str` for text, plain values for an `Option` column). An empty list matches no
    /// rows; `None` in the list on a nullable column matches NULL.
    pub fn is_in<I>(self, values: I) -> Expr<T>
    where
        I: IntoIterator,
        I::Item: ListValue<V>,
    {
        let (binds, null) = list_binds::<V, _>(values);
        let column = self.sql();
        match (binds.is_empty(), null) {
            (true, false) => Expr::new("0 = 1".to_owned(), vec![]),
            (true, true) => Expr::new(format!("{column} IS NULL"), vec![]),
            (false, false) => Expr::new(format!("{column} IN ({})", marks(binds.len())), binds),
            (false, true) => Expr::new(format!("({column} IN ({}) OR {column} IS NULL)", marks(binds.len())), binds),
        }
    }

    /// The column's value is none of `values`. An empty list matches every row. Like `ne`,
    /// a NULL never matches (SQL's `NOT IN`); keep those rows with `col.is_null().or(col.not_in(..))`.
    /// `None` in the list is ignored.
    pub fn not_in<I>(self, values: I) -> Expr<T>
    where
        I: IntoIterator,
        I::Item: ListValue<V>,
    {
        let (binds, null) = list_binds::<V, _>(values);
        let column = self.sql();
        match (binds.is_empty(), null) {
            (true, false) => Expr::new("1 = 1".to_owned(), vec![]),
            (true, true) => Expr::new(format!("{column} IS NOT NULL"), vec![]),
            // NOT IN already leaves out NULLs; a NULL bind would make it match nothing.
            (false, _) => Expr::new(format!("{column} NOT IN ({})", marks(binds.len())), binds),
        }
    }
}

/// `?, ?, ?`.
fn marks(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// A list's binds without its NULLs, and whether it had one.
fn list_binds<V, I>(values: I) -> (Vec<Value>, bool)
where
    I: IntoIterator,
    I::Item: ListValue<V>,
{
    let mut null = false;
    let mut binds = Vec::new();
    for value in values {
        match value.list_value() {
            Value::Null => null = true,
            value => binds.push(value),
        }
    }
    (binds, null)
}

/// A value [`Column::is_in`] and [`Column::not_in`] accept for a column of type `V`: a `V`
/// or `&V`, a `&str` for text, and a `V` or `&V` for an `Option<V>` column.
pub trait ListValue<V> {
    fn list_value(self) -> Value;
}

impl<V: ToValue> ListValue<V> for V {
    fn list_value(self) -> Value {
        self.to_value()
    }
}

impl<V: ToValue> ListValue<V> for &V {
    fn list_value(self) -> Value {
        self.to_value()
    }
}

impl<V: ToValue> ListValue<Option<V>> for V {
    fn list_value(self) -> Value {
        self.to_value()
    }
}

impl<V: ToValue> ListValue<Option<V>> for &V {
    fn list_value(self) -> Value {
        self.to_value()
    }
}

impl ListValue<String> for &str {
    fn list_value(self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl ListValue<Option<String>> for &str {
    fn list_value(self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl<T: Record, V> Column<T, Option<V>> {
    pub fn is_null(self) -> Expr<T> {
        Expr::new(format!("{} IS NULL", self.sql()), vec![])
    }

    pub fn is_not_null(self) -> Expr<T> {
        Expr::new(format!("{} IS NOT NULL", self.sql()), vec![])
    }
}

/// Text columns, `String` or `Option<String>` (a NULL never matches).
pub trait TextColumn {}
impl TextColumn for String {}
impl TextColumn for Option<String> {}

impl<T: Record, V: TextColumn> Column<T, V> {
    /// SQL `LIKE`: `%` is any run of characters, `_` any one, `\` escapes them; build
    /// patterns from user input with [`like::contains`] (and friends), which escape it.
    /// Postgres compares case-sensitively; SQLite ignores ASCII case. For the same answer
    /// everywhere, use [`Column::ilike`].
    pub fn like(self, pattern: impl AsRef<str>) -> Expr<T> {
        Expr::new(format!("{} LIKE ? ESCAPE '\\'", self.sql()), vec![Value::Text(pattern.as_ref().to_owned())])
    }

    /// Case-insensitive `LIKE` on every backend: `user::name.ilike(like::contains(&q))`.
    /// Written `LOWER(col) LIKE LOWER(?)`, so Postgres folds any letters and SQLite ASCII
    /// ones (as ILIKE and SQLite's LIKE do).
    pub fn ilike(self, pattern: impl AsRef<str>) -> Expr<T> {
        Expr::new(
            format!("LOWER({}) LIKE LOWER(?) ESCAPE '\\'", self.sql()),
            vec![Value::Text(pattern.as_ref().to_owned())],
        )
    }
}

/// `LIKE` patterns from user input, for [`Column::like`] and [`Column::ilike`]: `%`, `_` and
/// `\` in the input match only themselves.
///
/// ```ignore
/// Repository::query().filter(repository::name.ilike(kiln::like::contains(&params.q)))
/// ```
pub mod like {
    /// The input with `%`, `_` and `\` escaped, to put inside a pattern of your own.
    pub fn escape(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        for c in input.chars() {
            if matches!(c, '%' | '_' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// Matches text containing `input`.
    pub fn contains(input: &str) -> String {
        format!("%{}%", escape(input))
    }

    /// Matches text starting with `input`.
    pub fn starts_with(input: &str) -> String {
        format!("{}%", escape(input))
    }

    /// Matches text ending with `input`.
    pub fn ends_with(input: &str) -> String {
        format!("%{}", escape(input))
    }
}

/// A boolean SQL expression over record `T`. Filtering a `Query<Post>` with a
/// `Comment` column is a type error, unless the query joins `Comment`.
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

    fn erase(self) -> Clause {
        Clause { sql: self.sql, binds: self.binds }
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

/// A filter once it's in a query: its record type was checked on the way in.
#[derive(Clone)]
struct Clause {
    sql: String,
    binds: Vec<Value>,
}

/// The query's type parameters, held as a `fn` so they needn't be `Send`, `Sync` or `Clone`.
type QueryTypes<T, R, J> = fn() -> (T, R, J);

/// A query for records of type `T` with preloaded relations `R`, joining tables `J`.
///
/// `R` is a tuple of relation markers (`(post::author, post::comments)`). It only changes
/// through [`Query::preload`], and it decides the row type: plain `T` when nothing is
/// preloaded, `Loaded<T, R>` otherwise. `J` only changes through [`Query::join`] and
/// [`Query::left_join`], and decides which tables' columns the query accepts.
pub struct Query<T, R = (), J = ()> {
    filters: Vec<Clause>,
    orders: Vec<String>,
    /// `(table, JOIN clause)`.
    joins: Vec<(&'static str, String)>,
    limit: Option<u64>,
    offset: Option<u64>,
    lock: bool,
    _marker: PhantomData<QueryTypes<T, R, J>>,
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
impl<T, R, J> Clone for Query<T, R, J> {
    fn clone(&self) -> Self {
        Self {
            filters: self.filters.clone(),
            orders: self.orders.clone(),
            joins: self.joins.clone(),
            limit: self.limit,
            offset: self.offset,
            lock: self.lock,
            _marker: PhantomData,
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
        Self {
            filters: vec![],
            orders: vec![],
            joins: vec![],
            limit: None,
            offset: None,
            lock: false,
            _marker: PhantomData,
        }
    }
}

impl<T: Record, R, J> Query<T, R, J> {
    /// Keeps the rows matching `expr`, a condition on `T` or on a joined table.
    pub fn filter<U, I>(mut self, expr: Expr<U>) -> Self
    where
        U: InScope<T, J, I>,
    {
        self.filters.push(expr.erase());
        self
    }

    /// Sorts by a column of `T` or of a joined table; several orders apply in turn.
    pub fn order<U, I>(mut self, order: Order<U>) -> Self
    where
        U: InScope<T, J, I>,
    {
        self.orders.push(order.sql);
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
    pub fn preload<X>(self, _relation: X) -> Query<T, R::Output, J>
    where
        X: Relation<T>,
        R: Append<X>,
    {
        self.retype()
    }

    /// Joins a `belongs_to` or `has_one` relation's table (SQL `INNER JOIN`), so filters,
    /// orders and `select` can use its columns. Rows without a match are left out; the
    /// rows are still plain `T` records, one per record.
    ///
    /// ```ignore
    /// Repository::query()
    ///     .join(repository::namespace)
    ///     .filter(namespace::path.eq("afomera"))
    ///     .order(namespace::path.asc())
    /// // A relation of a joined table joins too:
    /// Issue::query().join(issue::repository).join(repository::namespace)
    /// ```
    ///
    /// A `has_many` doesn't join (it would repeat rows): filter through it with
    /// [`Column::in_query`]. Each table joins once; a query can't join its own table.
    pub fn join<X, I>(self, _relation: X) -> Query<T, R, J::Output>
    where
        X: Join,
        X::Owner: InScope<T, J, I>,
        J: AddJoin<Inner<X::Target>>,
    {
        self.add_join::<X, _>("INNER")
    }

    /// Like [`Query::join`], but keeps rows without a match (SQL `LEFT JOIN`), e.g. through
    /// an optional `belongs_to`. The joined columns are NULL on those rows: a filter on them
    /// doesn't match, and they can't be read with `select`.
    pub fn left_join<X, I>(self, _relation: X) -> Query<T, R, J::Output>
    where
        X: Join,
        X::Owner: InScope<T, J, I>,
        J: AddJoin<Left<X::Target>>,
    {
        self.add_join::<X, _>("LEFT")
    }

    fn add_join<X: Join, N>(mut self, kind: &str) -> Query<T, R, N> {
        self.joins.push((<X::Target as Record>::TABLE, join::clause::<X>(kind)));
        self.retype()
    }

    fn retype<R2, J2>(self) -> Query<T, R2, J2> {
        Query {
            filters: self.filters,
            orders: self.orders,
            joins: self.joins,
            limit: self.limit,
            offset: self.offset,
            lock: self.lock,
            _marker: PhantomData,
        }
    }

    /// Locks the rows `all`/`one`/`one_or_404` read until the transaction ends, so a
    /// read-then-write (handing out the next issue number) can't race. Only inside
    /// [`Db::transaction`]; preloaded relations and joined tables aren't locked.
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

    /// One column of the matching rows, of `T` or of a joined table: run it with
    /// [`Select::all`], or pass it to [`Column::in_query`] as a subquery.
    pub fn select<U, V, I>(self, column: Column<U, V>) -> Select<T, V, J>
    where
        U: Record + Selectable<T, J, I>,
    {
        self.select_sql(column.sql(), column.name())
    }

    fn select_sql<V>(self, sql: String, name: &'static str) -> Select<T, V, J> {
        Select { query: self.retype(), sql, name, _marker: PhantomData }
    }

    /// Joins are checked when the query runs: two tables of the same name would make
    /// every column of that table ambiguous.
    fn check_joins(&self) -> Result<()> {
        let mut tables = vec![T::TABLE];
        for (table, _) in &self.joins {
            if tables.contains(table) {
                return Err(Error::Config(format!(
                    "a query on `{}` joins `{table}` twice (or joins its own table); join each table once, filter through the other relation with `.in_query(..)`, or write the query with `db.sql(..)`",
                    T::TABLE
                )));
            }
            tables.push(table);
        }
        Ok(())
    }

    /// `FROM` with the joins, and `WHERE`.
    fn push_from_where(&self, sql: &mut String, binds: &mut Vec<Value>) {
        sql.push_str(&format!(" FROM \"{}\"", T::TABLE));
        for (_, clause) in &self.joins {
            sql.push(' ');
            sql.push_str(clause);
        }
        self.where_sql(sql, binds);
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
        let mut sql = format!("SELECT {select}");
        let mut binds = Vec::new();
        self.push_from_where(&mut sql, &mut binds);
        if ordered {
            sql.push_str(" ORDER BY ");
            if self.orders.is_empty() {
                sql.push_str(&format!("\"{}\".\"id\" ASC", T::TABLE));
            } else {
                sql.push_str(&self.orders.join(", "));
            }
        }
        match (self.limit, self.offset) {
            (Some(l), Some(o)) => sql.push_str(&format!(" LIMIT {l} OFFSET {o}")),
            (Some(l), None) => sql.push_str(&format!(" LIMIT {l}")),
            // SQLite needs a LIMIT before OFFSET, and Postgres refuses SQLite's `LIMIT -1`.
            (None, Some(o)) => sql.push_str(&format!(" LIMIT {} OFFSET {o}", i64::MAX)),
            (None, None) => {}
        }
        (sql, binds)
    }

    pub async fn count(self, db: &Db) -> Result<i64> {
        self.check_joins()?;
        let (sql, binds) = self.to_sql("COUNT(*)", false);
        let rows = db.fetch_all(&sql, binds).await?;
        let row = rows.first().ok_or(Error::NotFound)?;
        Ok(sqlx::Row::try_get(row, 0)?)
    }

    pub async fn exists(self, db: &Db) -> Result<bool> {
        Ok(self.limit(1).count(db).await? > 0)
    }

    /// Matching rows counted per value of `column` (of `T` or a joined table), in the
    /// column's order (NULL first):
    /// `Reaction::query().filter(..).count_by(reaction::emoji, &db)` gives `[("+1", 3), ..]`.
    /// Order, limit and offset are ignored.
    pub async fn count_by<U, V: FromColumn, I>(self, column: Column<U, V>, db: &Db) -> Result<Vec<(V, i64)>>
    where
        U: Record + Selectable<T, J, I>,
    {
        self.check_joins()?;
        let key = column.sql();
        let mut sql = format!("SELECT {key} AS \"key\", COUNT(*) AS \"count\"");
        let mut binds = Vec::new();
        self.push_from_where(&mut sql, &mut binds);
        // SQLite sorts NULL first and Postgres last; pin it so results match.
        sql.push_str(&format!(" GROUP BY {key} ORDER BY {key} NULLS FIRST"));
        let rows = db.fetch_all(&sql, binds).await?;
        let pairs = rows.iter().map(|row| Ok((V::from_column(row, "key")?, i64::from_column(row, "count")?)));
        pairs.collect::<std::result::Result<_, sqlx::Error>>().map_err(Error::from)
    }

    /// Sum of `column` over the matching rows; 0 when none match (NULLs are skipped).
    pub async fn sum<U, V: Summable, I>(self, column: Column<U, V>, db: &Db) -> Result<V::Total>
    where
        U: Record + InScope<T, J, I>,
    {
        // Postgres sums integers as NUMERIC, which sqlx's `Any` driver can't read: cast back.
        let select = format!("CAST(COALESCE(SUM({}), 0) AS {}) AS \"total\"", column.sql(), V::CAST);
        self.aggregate(&select, db).await
    }

    /// Largest value of `column` among the matching rows; `None` when none match.
    pub async fn max<U, V: MinMax, I>(self, column: Column<U, V>, db: &Db) -> Result<Option<V::Value>>
    where
        U: Record + InScope<T, J, I>,
    {
        self.aggregate(&format!("MAX({}) AS \"total\"", column.sql()), db).await
    }

    /// Smallest value of `column` among the matching rows; `None` when none match.
    pub async fn min<U, V: MinMax, I>(self, column: Column<U, V>, db: &Db) -> Result<Option<V::Value>>
    where
        U: Record + InScope<T, J, I>,
    {
        self.aggregate(&format!("MIN({}) AS \"total\"", column.sql()), db).await
    }

    async fn aggregate<V: FromColumn>(self, select: &str, db: &Db) -> Result<V> {
        self.check_joins()?;
        let mut sql = format!("SELECT {select}");
        let mut binds = Vec::new();
        self.push_from_where(&mut sql, &mut binds);
        let rows = db.fetch_all(&sql, binds).await?;
        Ok(V::from_column(rows.first().ok_or(Error::NotFound)?, "total")?)
    }

    /// Deletes every matching row of `T` (never of a joined table) and returns how many.
    /// Order and limit are ignored.
    pub async fn delete_all(mut self, db: &Db) -> Result<u64> {
        self.check_joins()?;
        let mut sql = format!("DELETE FROM \"{}\"", T::TABLE);
        let mut binds = Vec::new();
        if self.joins.is_empty() {
            self.where_sql(&mut sql, &mut binds);
        } else {
            // DELETE can't join portably; pick the rows with a subquery instead.
            let id = format!("\"{}\".\"id\"", T::TABLE);
            (self.limit, self.offset) = (None, None);
            let (select, select_binds) = self.to_sql(&id, false);
            sql.push_str(&format!(" WHERE {id} IN ({select})"));
            binds = select_binds;
        }
        sql.push_str(" RETURNING \"id\"");
        Ok(db.fetch_all(&sql, binds).await?.len() as u64)
    }
}

impl<T: Record, R: RelSet<T>, J> Query<T, R, J> {
    /// Runs the query plus one query per preloaded relation (never one per row).
    pub async fn all(self, db: &Db) -> Result<Vec<R::Row>> {
        self.check_joins()?;
        let select = T::COLUMNS.iter().map(|c| format!("\"{}\".\"{c}\" AS \"{c}\"", T::TABLE)).collect::<Vec<_>>();
        let (mut sql, binds) = self.to_sql(&select.join(", "), true);
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
    sql.push_str(&db.dialect().row_lock(T::TABLE));
    Ok(())
}

/// One column of a query's rows, from [`Query::select`].
pub struct Select<T, V, J = ()> {
    query: Query<T, (), J>,
    /// The qualified column.
    sql: String,
    name: &'static str,
    _marker: PhantomData<fn() -> V>,
}

/// A `select` keeps building like its query, so `.limit(..)`, `.order(..)` and `.filter(..)`
/// work before or after it.
impl<T: Record, V, J> Select<T, V, J> {
    pub fn filter<U, I>(mut self, expr: Expr<U>) -> Self
    where
        U: InScope<T, J, I>,
    {
        self.query = self.query.filter(expr);
        self
    }

    pub fn order<U, I>(mut self, order: Order<U>) -> Self
    where
        U: InScope<T, J, I>,
    {
        self.query = self.query.order(order);
        self
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.query = self.query.limit(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.query = self.query.offset(n);
        self
    }

    /// One-based page of `per` rows.
    pub fn page(mut self, page: u64, per: u64) -> Self {
        self.query = self.query.page(page, per);
        self
    }
}

impl<T: Record, V: FromColumn, J> Select<T, V, J> {
    /// The column's values, in the query's order (`pluck`).
    pub async fn all(self, db: &Db) -> Result<Vec<V>> {
        self.query.check_joins()?;
        let (sql, binds) = self.query.to_sql(&format!("{} AS \"{}\"", self.sql, self.name), true);
        let rows = db.fetch_all(&sql, binds).await?;
        rows.iter().map(|row| V::from_column(row, self.name).map_err(Error::from)).collect()
    }
}

impl<U: Record, V, J> Select<U, V, J> {
    fn subquery_sql(mut self) -> (String, Vec<Value>) {
        self.query.filters.push(Clause { sql: format!("{} IS NOT NULL", self.sql), binds: vec![] });
        // Order only matters when it picks which rows a limit keeps.
        let ordered = self.query.limit.is_some() || self.query.offset.is_some();
        self.query.to_sql(&self.sql, ordered)
    }
}

/// A subquery yielding values of type `V`, for [`Column::in_query`]: a
/// `Query<U>::select(column)` of that type, or a `Query<U>` itself, which yields its ids.
/// `Option<V>` and `V` mix either way: the subquery skips NULLs.
pub trait Subquery<V> {
    /// The `SELECT` and its binds.
    fn into_sql(self) -> (String, Vec<Value>);
}

impl<U: Record, V, J> Subquery<V> for Select<U, V, J> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, V, J> Subquery<Option<V>> for Select<U, V, J> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, V, J> Subquery<V> for Select<U, Option<V>, J> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.subquery_sql()
    }
}

impl<U: Record, R, J> Subquery<Id<U>> for Query<U, R, J> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.select_sql::<Id<U>>(Column::<U, Id<U>>::new("id").sql(), "id").subquery_sql()
    }
}

impl<U: Record, R, J> Subquery<Option<Id<U>>> for Query<U, R, J> {
    fn into_sql(self) -> (String, Vec<Value>) {
        self.select_sql::<Id<U>>(Column::<U, Id<U>>::new("id").sql(), "id").subquery_sql()
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

#[must_use = "an insert does nothing until it's run with `.run(&db)`"]
pub struct Insert<T> {
    values: Vec<(&'static str, Value)>,
    /// `updated_at` was set explicitly, so a `created_at` override leaves it alone.
    updated_at_set: bool,
    /// A timestamp override for a column the record doesn't have.
    missing: Option<&'static str>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Record> Insert<T> {
    pub(crate) fn new(values: Vec<(&'static str, Value)>) -> Self {
        Self { values, updated_at_set: false, missing: None, _marker: PhantomData }
    }

    /// Stores `ts` as the row's `created_at` instead of now (back-dated seeds, imports).
    /// `updated_at` follows it unless also set.
    pub fn created_at(mut self, ts: Timestamp) -> Self {
        self.set_timestamp("created_at", ts);
        if !self.updated_at_set && self.values.iter().any(|(n, _)| *n == "updated_at") {
            self.set_timestamp("updated_at", ts);
        }
        self
    }

    /// Stores `ts` as the row's `updated_at` instead of now (or the `created_at` override).
    pub fn updated_at(mut self, ts: Timestamp) -> Self {
        self.set_timestamp("updated_at", ts);
        self.updated_at_set = true;
        self
    }

    fn set_timestamp(&mut self, column: &'static str, ts: Timestamp) {
        match self.values.iter_mut().find(|(n, _)| *n == column) {
            Some((_, value)) => *value = ts.to_value(),
            None => self.missing = Some(column),
        }
    }

    fn check(&self) -> Result<()> {
        match self.missing {
            Some(column) => Err(Error::Config(format!(
                "`{}` has no `{column}` column to set at insert; add `pub {column}: Timestamp` to the record",
                T::TABLE
            ))),
            None => Ok(()),
        }
    }

    /// Inserts the row and returns the stored record.
    pub async fn run(self, db: &Db) -> Result<T> {
        self.check()?;
        let names: Vec<String> = self.values.iter().map(|(n, _)| format!("\"{n}\"")).collect();
        let sql = format!(
            "INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING {}",
            T::TABLE,
            names.join(", "),
            marks(self.values.len()),
            quoted_columns::<T>()
        );
        let binds = self.values.into_iter().map(|(_, v)| v).collect();
        let rows = db.fetch_all(&sql, binds).await?;
        Ok(T::from_row(rows.first().ok_or(Error::NotFound)?)?)
    }
}

/// A row [`Record::insert_all`] takes: a `New{Record}`, or an [`Insert`] for one with
/// its own timestamps (`Post::insert(new).created_at(ts)`).
pub trait IntoInsert<T> {
    fn into_insert(self) -> Insert<T>;
}

impl<T> IntoInsert<T> for Insert<T> {
    fn into_insert(self) -> Insert<T> {
        self
    }
}

/// Many rows in one statement per chunk, from [`Record::insert_all`].
#[must_use = "an insert does nothing until it's run with `.run(&db)`"]
pub struct InsertAll<T> {
    rows: Vec<Insert<T>>,
}

impl<T: Record> InsertAll<T> {
    pub(crate) fn new(rows: Vec<Insert<T>>) -> Self {
        Self { rows }
    }

    /// Stores `ts` as every row's `created_at` (and `updated_at`, unless a row sets its own).
    pub fn created_at(mut self, ts: Timestamp) -> Self {
        self.rows = self.rows.into_iter().map(|row| row.created_at(ts)).collect();
        self
    }

    /// Inserts the rows and returns them as stored, in the order given. Each chunk is one
    /// multi-row `INSERT .. RETURNING` (sized to the backend's limit on binds); several
    /// chunks run in a transaction, so either every row goes in or none does. On a
    /// transaction handle they're part of it.
    pub async fn run(self, db: &Db) -> Result<Vec<T>> {
        for row in &self.rows {
            row.check()?;
        }
        let Some(first) = self.rows.first() else { return Ok(vec![]) };
        let columns: Vec<&'static str> = first.values.iter().map(|(n, _)| *n).collect();
        if self.rows.iter().any(|row| row.values.iter().map(|(n, _)| *n).ne(columns.iter().copied())) {
            return Err(Error::Integrity(format!("rows for `{}` don't all set the same columns", T::TABLE)));
        }
        let per_chunk = (db.dialect().max_binds() / columns.len().max(1)).clamp(1, 1000);
        let chunks: Vec<Vec<Value>> = self
            .rows
            .chunks(per_chunk)
            .map(|chunk| chunk.iter().flat_map(|row| row.values.iter().map(|(_, v)| v.clone())).collect())
            .collect();
        let row_marks = format!("({})", marks(columns.len()));
        let names = columns.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
        let statements: Vec<(String, Vec<Value>)> = chunks
            .into_iter()
            .map(|binds| {
                let rows = binds.len() / columns.len().max(1);
                let values = vec![row_marks.as_str(); rows].join(", ");
                let sql = format!(
                    "INSERT INTO \"{}\" ({names}) VALUES {values} RETURNING {}",
                    T::TABLE,
                    quoted_columns::<T>()
                );
                (sql, binds)
            })
            .collect();
        let single = statements.len() == 1;
        let run = |db: Db| async move {
            let mut records = Vec::new();
            for (sql, binds) in statements {
                let rows = db.fetch_all(&sql, binds).await?;
                let mut chunk = rows.iter().map(T::from_row).collect::<std::result::Result<Vec<T>, _>>()?;
                // RETURNING's row order isn't promised; ids are handed out in VALUES order.
                chunk.sort_by_key(|record| record.id().get());
                records.extend(chunk);
            }
            Ok(records)
        };
        if single || db.in_transaction() { run(db.clone()).await } else { db.transaction(run).await }
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
