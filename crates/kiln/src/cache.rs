//! Caching, in the app's database (like Rails' Solid Cache): no Redis to run, and it
//! survives restarts and deploys.
//!
//! ```ignore
//! let stats = ctx.cache().fetch("dashboard:stats", SignedDuration::from_mins(5), || async {
//!     Stats::compute(&ctx.db).await                       // runs only on a miss
//! }).await?;
//!
//! ctx.cache().delete("dashboard:stats").await?;           // after a write that changes it
//! let hits = ctx.cache().increment("hits:/posts", 1, SignedDuration::from_hours(1)).await?;
//! ```
//!
//! Values are anything `Serialize + Deserialize`, stored as JSON. Entries expire after
//! their time to live; workers delete expired ones. Keys are strings up to 250 characters:
//! put a version in keys whose shape changes (`"v2:dashboard:stats"`), and the record's
//! `updated_at` in keys for a record (`format!("post:{}:{}", post.id, post.updated_at)`).
//! Two requests that miss at once both compute; the later write wins.

use std::future::Future;

use jiff::SignedDuration;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::Row;

use crate::{Ctx, Db, Error, Result, Timestamp, Value};

/// The longest key.
pub const MAX_KEY: usize = 250;

/// The app's cache. Get it with `ctx.cache()`.
#[derive(Clone)]
pub struct Cache {
    db: Db,
}

impl Ctx {
    /// The app's cache (in its database).
    pub fn cache(&self) -> Cache {
        Cache { db: self.db.role(crate::db::Role::Cache) }
    }
}

fn now() -> i64 {
    Timestamp::now().as_jiff().as_microsecond()
}

fn expiry(ttl: SignedDuration) -> i64 {
    (Timestamp::now() + ttl).as_jiff().as_microsecond()
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY {
        return Err(Error::BadRequest(format!("cache keys are 1 to {MAX_KEY} characters (got {})", key.len())));
    }
    Ok(())
}

impl Cache {
    pub(crate) fn for_db(db: &Db) -> Self {
        Self { db: db.role(crate::db::Role::Cache) }
    }

    /// The value at `key`, computing and storing it with `compute` on a miss.
    pub async fn fetch<T, F, Fut>(&self, key: &str, ttl: SignedDuration, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        if let Some(value) = self.get(key).await? {
            return Ok(value);
        }
        let value = compute().await?;
        self.set(key, &value, ttl).await?;
        Ok(value)
    }

    /// The value at `key`, if it's there and hasn't expired. A stored value that no longer
    /// deserializes as `T` (its shape changed) counts as a miss.
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        check_key(key)?;
        let rows = self
            .db
            .fetch_all(
                "SELECT \"value\" FROM \"kiln_cache\" WHERE \"key\" = ? AND \"expires_at\" > ?",
                vec![Value::Text(key.to_owned()), Value::Integer(now())],
            )
            .await?;
        Ok(rows.first().and_then(|r| serde_json::from_str(&r.get::<String, _>(0)).ok()))
    }

    /// Stores `value` at `key` for `ttl`.
    pub async fn set<T: Serialize + ?Sized>(&self, key: &str, value: &T, ttl: SignedDuration) -> Result<()> {
        check_key(key)?;
        let json = serde_json::to_string(value).map_err(|e| Error::BadRequest(format!("cache value: {e}")))?;
        self.db
            .fetch_all(
                "INSERT INTO \"kiln_cache\" (\"key\", \"value\", \"expires_at\", \"created_at\") VALUES (?, ?, ?, ?) \
                 ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\", \"expires_at\" = excluded.\"expires_at\", \
                 \"created_at\" = excluded.\"created_at\" RETURNING \"key\"",
                vec![Value::Text(key.to_owned()), Value::Text(json), Value::Integer(expiry(ttl)), Value::Integer(now())],
            )
            .await?;
        Ok(())
    }

    /// Removes `key`. Whether it was there.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        check_key(key)?;
        let rows = self
            .db
            .fetch_all(
                "DELETE FROM \"kiln_cache\" WHERE \"key\" = ? RETURNING \"key\"",
                vec![Value::Text(key.to_owned())],
            )
            .await?;
        Ok(!rows.is_empty())
    }

    /// Removes every key starting with `prefix` (`"post:12:"`). How many went.
    pub async fn delete_prefix(&self, prefix: &str) -> Result<u64> {
        let pattern = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let rows = self
            .db
            .fetch_all(
                "DELETE FROM \"kiln_cache\" WHERE \"key\" LIKE ? ESCAPE '\\' RETURNING \"key\"",
                vec![Value::Text(pattern)],
            )
            .await?;
        Ok(rows.len() as u64)
    }

    /// Adds `by` to the counter at `key` and returns the new count. A missing or expired
    /// counter starts from zero and lives for `ttl` from now; counting doesn't extend it.
    /// Atomic, so concurrent requests each get their own number.
    pub async fn increment(&self, key: &str, by: i64, ttl: SignedDuration) -> Result<i64> {
        check_key(key)?;
        let now = now();
        let rows = self
            .db
            .fetch_all(
                "INSERT INTO \"kiln_cache\" (\"key\", \"value\", \"expires_at\", \"created_at\") VALUES (?, ?, ?, ?) \
                 ON CONFLICT (\"key\") DO UPDATE SET \
                 \"value\" = CASE WHEN \"kiln_cache\".\"expires_at\" > ? \
                     THEN CAST(CAST(\"kiln_cache\".\"value\" AS BIGINT) + ? AS TEXT) ELSE excluded.\"value\" END, \
                 \"expires_at\" = CASE WHEN \"kiln_cache\".\"expires_at\" > ? THEN \"kiln_cache\".\"expires_at\" ELSE excluded.\"expires_at\" END \
                 RETURNING \"value\"",
                vec![
                    Value::Text(key.to_owned()),
                    Value::Text(by.to_string()),
                    Value::Integer(expiry(ttl)),
                    Value::Integer(now),
                    Value::Integer(now),
                    Value::Integer(by),
                    Value::Integer(now),
                ],
            )
            .await?;
        rows.first()
            .and_then(|r| r.get::<String, _>(0).parse().ok())
            .ok_or_else(|| Error::BadRequest(format!("cache key `{key}` holds something other than a count")))
    }

    /// Empties the cache. How many entries went.
    pub async fn clear(&self) -> Result<u64> {
        let rows = self.db.fetch_all("DELETE FROM \"kiln_cache\" RETURNING \"key\"", vec![]).await?;
        Ok(rows.len() as u64)
    }
}

/// Deletes expired entries (workers do this every few minutes). How many went.
pub async fn purge_expired(db: &Db) -> Result<u64> {
    let rows = db
        .role(crate::db::Role::Cache)
        .fetch_all(
            "DELETE FROM \"kiln_cache\" WHERE \"expires_at\" <= ? RETURNING \"key\"",
            vec![Value::Integer(now())],
        )
        .await?;
    Ok(rows.len() as u64)
}

/// What `/kiln/cache` shows.
#[derive(Debug, Clone, Serialize)]
pub struct CacheEntry {
    pub key: String,
    pub bytes: i64,
    pub expires_at: Timestamp,
    pub created_at: Timestamp,
}

impl Db {
    pub(crate) async fn with_cache_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_cache\" (\"key\" TEXT PRIMARY KEY, \"value\" TEXT NOT NULL, \
             \"expires_at\" INTEGER NOT NULL, \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.execute("CREATE INDEX IF NOT EXISTS \"kiln_cache_expires_at\" ON \"kiln_cache\" (\"expires_at\")").await?;
        Ok(self)
    }

    /// Live entries, newest first, optionally only keys containing `search`.
    pub async fn cache_entries(&self, search: &str, limit: i64) -> Result<Vec<CacheEntry>> {
        let at = |us: i64| Timestamp::from_jiff(jiff::Timestamp::from_microsecond(us).unwrap_or_default());
        let rows = self
            .role(crate::db::Role::Cache)
            .fetch_all(
                "SELECT \"key\", CAST(LENGTH(\"value\") AS BIGINT), \"expires_at\", \"created_at\" FROM \"kiln_cache\" \
                 WHERE \"expires_at\" > ? AND LOWER(\"key\") LIKE LOWER(?) ORDER BY \"created_at\" DESC LIMIT ?",
                vec![Value::Integer(now()), Value::Text(format!("%{search}%")), Value::Integer(limit)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| CacheEntry {
                key: r.get(0),
                bytes: r.get::<i64, _>(1),
                expires_at: at(r.get(2)),
                created_at: at(r.get(3)),
            })
            .collect())
    }
}
