use std::{
    future::Future,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use sqlx::{
    Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};
use tokio::sync::Mutex;

use crate::{Error, Result, Value};

/// A database handle. Cheap to clone. Inside [`Db::transaction`] the handle is bound to
/// the transaction, so the same query code runs either way. SQLite only for now.
#[derive(Clone)]
pub struct Db {
    conn: Conn,
    queries: Arc<AtomicU64>,
    /// Inside a transaction: work to run once it commits (e.g. waking stream listeners).
    after_commit: Option<Arc<std::sync::Mutex<Vec<AfterCommit>>>>,
}

type AfterCommit = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
enum Conn {
    Pool(SqlitePool),
    Tx(Arc<Mutex<Option<Transaction<'static, Sqlite>>>>),
}

impl Db {
    /// Connects to a SQLite database URL such as `sqlite://app.db`, creating the file if needed.
    pub async fn connect(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true).foreign_keys(true);
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        Self::from_pool(pool).with_kiln_tables().await
    }

    /// A private in-memory database, kept alive for the lifetime of this handle.
    ///
    /// sqlx gives each `sqlite::memory:` database a unique name with shared cache, so the
    /// pool's connections all see the same data and queries run concurrently. Connections
    /// never expire, so the database lives as long as the pool.
    pub async fn memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).with_kiln_tables().await
    }

    fn from_pool(pool: SqlitePool) -> Self {
        Self { conn: Conn::Pool(pool), queries: Arc::new(AtomicU64::new(0)), after_commit: None }
    }

    /// Runs `f` in a transaction: committed when it returns `Ok`, rolled back on `Err` or
    /// panic. `f` gets a handle bound to the transaction. Nested calls join the outer one.
    ///
    /// ```ignore
    /// let post = ctx.db.transaction(|tx| async move {
    ///     let post = Post::insert(new).run(&tx).await?;
    ///     Comment::insert(first_comment(&post)).run(&tx).await?;
    ///     Ok(post)
    /// }).await?;
    /// ```
    pub async fn transaction<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T>> + Send,
        T: Send,
    {
        let pool = match &self.conn {
            Conn::Tx(_) => return f(self.clone()).await,
            Conn::Pool(pool) => pool,
        };
        let handle = Arc::new(Mutex::new(Some(pool.begin().await?)));
        let hooks: Arc<std::sync::Mutex<Vec<AfterCommit>>> = Arc::default();
        let tx_db =
            Db { conn: Conn::Tx(handle.clone()), queries: self.queries.clone(), after_commit: Some(hooks.clone()) };
        let result = f(tx_db).await;
        let tx = handle.lock().await.take().ok_or_else(|| Error::Integrity("transaction already finished".into()))?;
        match result {
            Ok(value) => {
                tx.commit().await?;
                let hooks = std::mem::take(&mut *hooks.lock().unwrap_or_else(|e| e.into_inner()));
                for hook in hooks {
                    hook();
                }
                Ok(value)
            }
            Err(e) => {
                tx.rollback().await?;
                Err(e)
            }
        }
    }

    /// Runs `f` after the surrounding transaction commits, or now outside one. Dropped if
    /// the transaction rolls back.
    pub(crate) fn after_commit(&self, f: impl FnOnce() + Send + 'static) {
        match &self.after_commit {
            Some(hooks) => hooks.lock().unwrap_or_else(|e| e.into_inner()).push(Box::new(f)),
            None => f(),
        }
    }

    /// A dedicated connection from the pool, for work that must stay on one connection.
    pub(crate) async fn acquire(&self) -> Result<sqlx::pool::PoolConnection<Sqlite>> {
        match &self.conn {
            Conn::Pool(pool) => Ok(pool.acquire().await?),
            Conn::Tx(_) => Err(Error::Integrity("can't take a separate connection inside a transaction".into())),
        }
    }

    /// Runs one statement.
    pub async fn execute(&self, sql: &str) -> Result<()> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "kiln::sql", "{sql}");
        let query = sqlx::query(sql);
        match &self.conn {
            Conn::Pool(pool) => query.execute(pool).await?,
            Conn::Tx(tx) => query.execute(&mut **tx_mut(&mut *tx.lock().await)?).await?,
        };
        Ok(())
    }

    /// Runs a script of several statements on one connection, e.g. a migration.
    pub async fn execute_script(&self, sql: &str) -> Result<()> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "kiln::sql", "{sql}");
        let script = sqlx::raw_sql(sql);
        match &self.conn {
            Conn::Pool(pool) => script.execute(pool).await?,
            Conn::Tx(tx) => script.execute(&mut **tx_mut(&mut *tx.lock().await)?).await?,
        };
        Ok(())
    }

    pub(crate) async fn fetch_all(&self, sql: &str, binds: Vec<Value>) -> Result<Vec<SqliteRow>> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "kiln::sql", "{sql}");
        let mut query = sqlx::query(sql);
        for value in binds {
            query = match value {
                Value::Null => query.bind(None::<i64>),
                Value::Integer(i) => query.bind(i),
                Value::Real(f) => query.bind(f),
                Value::Text(s) => query.bind(s),
            };
        }
        Ok(match &self.conn {
            Conn::Pool(pool) => query.fetch_all(pool).await?,
            Conn::Tx(tx) => query.fetch_all(&mut **tx_mut(&mut *tx.lock().await)?).await?,
        })
    }

    /// Number of statements run through this handle. Tests use it to prove preloading
    /// costs one query per relation, not one per row.
    pub fn query_count(&self) -> u64 {
        self.queries.load(Ordering::Relaxed)
    }
}

fn tx_mut<'a>(slot: &'a mut Option<Transaction<'static, Sqlite>>) -> Result<&'a mut Transaction<'static, Sqlite>> {
    slot.as_mut().ok_or_else(|| Error::Integrity("the transaction this handle belongs to has finished".into()))
}
