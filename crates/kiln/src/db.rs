use std::{
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

use crate::{Record, Result, Value};

/// A database handle. Cheap to clone. M0 supports SQLite only.
#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
    queries: Arc<AtomicU64>,
}

impl Db {
    /// Connects to a SQLite database URL such as `sqlite://app.db`, creating the file if needed.
    pub async fn connect(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true).foreign_keys(true);
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        Ok(Self::from_pool(pool))
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
        Ok(Self::from_pool(pool))
    }

    fn from_pool(pool: SqlitePool) -> Self {
        Self { pool, queries: Arc::new(AtomicU64::new(0)) }
    }

    /// Creates the table and indexes for a record from its struct definition.
    /// M1 replaces this with generated, reviewed migrations (`kiln db diff`).
    pub async fn create_table<T: Record>(&self) -> Result<()> {
        for statement in T::schema().create_sql() {
            self.execute(&statement).await?;
        }
        Ok(())
    }

    pub async fn execute(&self, sql: &str) -> Result<()> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "kiln::sql", "{sql}");
        sqlx::query(sql).execute(&self.pool).await?;
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
        Ok(query.fetch_all(&self.pool).await?)
    }

    /// Number of statements run through this handle. Tests use it to prove preloading
    /// costs one query per relation, not one per row.
    pub fn query_count(&self) -> u64 {
        self.queries.load(Ordering::Relaxed)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
