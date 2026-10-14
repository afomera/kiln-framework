use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use sqlx::{
    Any, AnyPool, Transaction,
    any::{AnyPoolOptions, AnyRow},
};
use tokio::sync::Mutex;

use crate::{Error, Result, Value, dialect::Dialect};

/// A row from any backend. Records decode themselves from it (`Record::from_row`).
pub type Row = AnyRow;

/// A database handle. Cheap to clone. Inside [`Db::transaction`] the handle is bound to
/// the transaction, so the same query code runs either way. SQLite or Postgres, chosen by
/// the URL; what differs between them lives in [`crate::dialect`].
#[derive(Clone)]
pub struct Db {
    conn: Conn,
    dialect: &'static dyn Dialect,
    queries: Arc<AtomicU64>,
    /// Inside a transaction: work to run once it commits (e.g. waking stream listeners) or
    /// rolls back.
    after_commit: Option<Arc<std::sync::Mutex<Hooks>>>,
    /// Separate databases for Kiln's job queue, cache and broadcasts, when the app has them.
    roles: Arc<Roles>,
}

type AfterCommit = Box<dyn FnOnce() + Send>;
type AsyncHook = Box<dyn FnOnce() -> crate::BoxFuture<'static, ()> + Send>;

#[derive(Default)]
struct Hooks {
    commit: Vec<AfterCommit>,
    commit_async: Vec<AsyncHook>,
    rollback: Vec<AsyncHook>,
}

/// Where Kiln's own heavy tables live. `None`: in the app's database.
#[derive(Clone, Default)]
struct Roles {
    queue: Option<Db>,
    cache: Option<Db>,
    streams: Option<Db>,
}

/// One of Kiln's tables that can live in a database of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Jobs, batches, schedules, queue pauses and limits (`QUEUE_DATABASE_URL`).
    Queue,
    /// `ctx.cache()` (`CACHE_DATABASE_URL`).
    Cache,
    /// The broadcast log realtime pages read (`STREAMS_DATABASE_URL`).
    Streams,
}

#[derive(Clone)]
enum Conn {
    Pool(AnyPool),
    Tx(Arc<Mutex<Option<Transaction<'static, Any>>>>),
}

impl Db {
    /// Connects to `sqlite://app.db` (created if missing) or `postgres://user@host/db`.
    pub async fn connect(url: &str) -> Result<Self> {
        let dialect = crate::dialect::for_url(url)?;
        sqlx::any::install_default_drivers();
        let url = if dialect.name() == "sqlite" && !url.contains("mode=") {
            format!("{url}{}mode=rwc", if url.contains('?') { "&" } else { "?" })
        } else {
            url.to_owned()
        };
        let setup = dialect.on_connect();
        let pool = AnyPoolOptions::new()
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    if let Some(sql) = setup {
                        sqlx::query(sql).execute(conn).await?;
                    }
                    Ok(())
                })
            })
            .connect(&url)
            .await?;
        Self::from_pool(pool, dialect).with_kiln_tables().await
    }

    /// Like [`Db::connect`], but creates a missing Postgres database first (SQLite files
    /// are always created).
    pub async fn connect_or_create(url: &str) -> Result<Self> {
        match Self::connect(url).await {
            Err(Error::Database(e))
                if crate::dialect::for_url(url)?.name() == "postgres" && e.to_string().contains("does not exist") =>
            {
                create_postgres_database(url).await?;
                Self::connect(url).await
            }
            other => other,
        }
    }

    /// A Postgres database confined to a fresh schema (tests run each in its own).
    pub(crate) async fn postgres_schema(url: &str, schema: &str) -> Result<Self> {
        sqlx::any::install_default_drivers();
        // Make sure the database exists and the schema is there.
        let setup = Self::connect_or_create(url).await?;
        setup.execute_script(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\"")).await?;
        let search_path = format!("SET search_path TO \"{schema}\"");
        let pool = AnyPoolOptions::new()
            .max_connections(5)
            .after_connect(move |conn, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(&mut *conn).await?;
                    if let Some(sql) = crate::dialect::Postgres.on_connect() {
                        sqlx::query(sql).execute(&mut *conn).await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Self::from_pool(pool, &crate::dialect::Postgres).with_kiln_tables().await
    }

    /// A private in-memory SQLite database, kept alive for the lifetime of this handle.
    ///
    /// Each one gets a unique shared-cache name, so the pool's connections all see the
    /// same data. Connections never expire, so the database lives as long as the pool.
    pub async fn memory() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        sqlx::any::install_default_drivers();
        let name = format!("kiln-memory-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let pool = AnyPoolOptions::new()
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&format!("sqlite:file:{name}?mode=memory&cache=shared"))
            .await?;
        Self::from_pool(pool, &crate::dialect::Sqlite).with_kiln_tables().await
    }

    fn from_pool(pool: AnyPool, dialect: &'static dyn Dialect) -> Self {
        Self {
            conn: Conn::Pool(pool),
            dialect,
            queries: Arc::new(AtomicU64::new(0)),
            after_commit: None,
            roles: Arc::default(),
        }
    }

    /// Keeps `role`'s tables (jobs, cache or broadcasts) in `db` instead of this database,
    /// so each can be sized and scaled on its own. `kiln::Cli` sets these from
    /// `QUEUE_DATABASE_URL`, `CACHE_DATABASE_URL` and `STREAMS_DATABASE_URL`.
    ///
    /// Jobs enqueued and broadcasts sent inside a transaction on this database are then
    /// handed over when it commits rather than in the same transaction: a rolled-back write
    /// still never enqueues or broadcasts, but a crash in the instant after the commit can
    /// lose them (the same trade-off as Rails with a separate Solid Queue database).
    pub fn with_role(mut self, role: Role, db: Db) -> Self {
        let mut roles = (*self.roles).clone();
        // The role's own handle has no roles, so its queries stay on it.
        let db = Db { roles: Arc::default(), ..db };
        match role {
            Role::Queue => roles.queue = Some(db),
            Role::Cache => roles.cache = Some(db),
            Role::Streams => roles.streams = Some(db),
        }
        self.roles = Arc::new(roles);
        self
    }

    /// The database holding `role`'s tables: this one unless the app split it out.
    pub fn role(&self, role: Role) -> Db {
        let split = match role {
            Role::Queue => &self.roles.queue,
            Role::Cache => &self.roles.cache,
            Role::Streams => &self.roles.streams,
        };
        split.clone().unwrap_or_else(|| self.clone())
    }

    /// Whether `role` lives in another database while this handle is inside a
    /// transaction: work for it must be handed over when the transaction commits.
    pub(crate) fn hands_over(&self, role: Role) -> bool {
        self.after_commit.is_some()
            && match role {
                Role::Queue => self.roles.queue.is_some(),
                Role::Cache => self.roles.cache.is_some(),
                Role::Streams => self.roles.streams.is_some(),
            }
    }

    /// Whether `role`'s tables are in a database of their own.
    pub fn is_split(&self, role: Role) -> bool {
        match role {
            Role::Queue => self.roles.queue.is_some(),
            Role::Cache => self.roles.cache.is_some(),
            Role::Streams => self.roles.streams.is_some(),
        }
    }

    /// The backend this database runs on.
    pub fn dialect(&self) -> &'static dyn Dialect {
        self.dialect
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
        let hooks: Arc<std::sync::Mutex<Hooks>> = Arc::default();
        let tx_db = Db {
            conn: Conn::Tx(handle.clone()),
            dialect: self.dialect,
            queries: self.queries.clone(),
            after_commit: Some(hooks.clone()),
            roles: self.roles.clone(),
        };
        let result = f(tx_db).await;
        let tx = handle.lock().await.take().ok_or_else(|| Error::Integrity("transaction already finished".into()))?;
        match result {
            Ok(value) => {
                let committed = tx.commit().await;
                let hooks = std::mem::take(&mut *hooks.lock().unwrap_or_else(|e| e.into_inner()));
                if let Err(e) = committed {
                    for hook in hooks.rollback {
                        hook().await;
                    }
                    return Err(e.into());
                }
                for hook in hooks.commit_async {
                    hook().await;
                }
                for hook in hooks.commit {
                    hook();
                }
                Ok(value)
            }
            Err(e) => {
                tx.rollback().await?;
                let hooks = std::mem::take(&mut *hooks.lock().unwrap_or_else(|e| e.into_inner()));
                for hook in hooks.rollback {
                    hook().await;
                }
                Err(e)
            }
        }
    }

    /// Runs `f` after the surrounding transaction commits, or now outside one. Dropped if
    /// the transaction rolls back.
    pub(crate) fn after_commit(&self, f: impl FnOnce() + Send + 'static) {
        match &self.after_commit {
            Some(hooks) => hooks.lock().unwrap_or_else(|e| e.into_inner()).commit.push(Box::new(f)),
            None => f(),
        }
    }

    /// Async work for once the surrounding transaction commits (finished before
    /// `transaction` returns), or now outside one.
    pub(crate) async fn after_commit_async<F>(&self, f: impl FnOnce() -> F + Send + 'static)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match &self.after_commit {
            Some(hooks) => hooks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .commit_async
                .push(Box::new(move || Box::pin(f()) as crate::BoxFuture<'static, ()>)),
            None => f().await,
        }
    }

    /// Async work for if the surrounding transaction rolls back (nothing outside one).
    pub(crate) fn after_rollback<F>(&self, f: impl FnOnce() -> F + Send + 'static)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(hooks) = &self.after_commit {
            hooks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .rollback
                .push(Box::new(move || Box::pin(f()) as crate::BoxFuture<'static, ()>));
        }
    }

    /// A dedicated connection from the pool, for work that must stay on one connection.
    pub(crate) async fn acquire(&self) -> Result<sqlx::pool::PoolConnection<Any>> {
        match &self.conn {
            Conn::Pool(pool) => Ok(pool.acquire().await?),
            Conn::Tx(_) => Err(Error::Integrity("can't take a separate connection inside a transaction".into())),
        }
    }

    /// Runs one DDL statement, written SQLite-style and translated for the backend.
    pub async fn execute(&self, sql: &str) -> Result<()> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        let sql = self.dialect.ddl(sql);
        tracing::debug!(target: "kiln::sql", "{sql}");
        capture(&sql);
        let query = sqlx::query(&sql);
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
        capture(sql);
        let script = sqlx::raw_sql(sql);
        match &self.conn {
            Conn::Pool(pool) => script.execute(pool).await?,
            Conn::Tx(tx) => script.execute(&mut **tx_mut(&mut *tx.lock().await)?).await?,
        };
        Ok(())
    }

    pub(crate) async fn fetch_all(&self, sql: &str, binds: Vec<Value>) -> Result<Vec<Row>> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        let (sql, binds) = self.dialect.prepare(sql, binds);
        tracing::debug!(target: "kiln::sql", "{sql}");
        capture(&sql);
        // Prepared statements are cached per connection, with their columns. Outside
        // production, migrations run while the server does, and a cached statement would
        // keep reading the old columns ("no column found"), so nothing is cached there.
        let mut query = sqlx::query(&sql).persistent(crate::config::is_production());
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

tokio::task_local! {
    static CAPTURED: std::sync::Arc<std::sync::Mutex<Vec<String>>>;
}

/// Runs `f`, collecting every SQL statement it runs on this task (`<app> request`, for
/// agents checking what a page queried).
pub(crate) async fn capturing_sql<F: Future>(f: F) -> (F::Output, Vec<String>) {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = CAPTURED.scope(log.clone(), f).await;
    let statements = std::mem::take(&mut *log.lock().unwrap_or_else(|e| e.into_inner()));
    (out, statements)
}

fn capture(sql: &str) {
    let _ = CAPTURED.try_with(|log| log.lock().unwrap_or_else(|e| e.into_inner()).push(sql.to_owned()));
}

fn tx_mut<'a>(slot: &'a mut Option<Transaction<'static, Any>>) -> Result<&'a mut Transaction<'static, Any>> {
    slot.as_mut().ok_or_else(|| Error::Integrity("the transaction this handle belongs to has finished".into()))
}

/// `CREATE DATABASE` for a Postgres URL's database, connecting to `postgres` to do it.
async fn create_postgres_database(url: &str) -> Result<()> {
    let (base, rest) = url.rsplit_once('/').ok_or_else(|| Error::Config(format!("`{url}` names no database")))?;
    let name = rest.split('?').next().unwrap_or(rest);
    let query = rest.strip_prefix(name).unwrap_or("");
    let admin = Db::connect(&format!("{base}/postgres{query}")).await?;
    admin.execute_script(&format!("CREATE DATABASE \"{}\"", name.replace('"', ""))).await?;
    tracing::info!(target: "kiln::db", database = name, "created the database");
    Ok(())
}
