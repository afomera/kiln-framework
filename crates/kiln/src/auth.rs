//! Authentication: database-backed sign-ins, password hashing and signed tokens.
//!
//! **Sign-ins.** `session.sign_in(&ctx.db, &user).await?` stores a row in `kiln_sessions`
//! and puts a random token in the (signed) session cookie; the row keeps the token's
//! SHA-256, never the token. `Current<User>` finds the row, then the user. Because sign-ins
//! are rows, people can see where they're signed in and sign devices out:
//!
//! ```ignore
//! let sign_ins = session.sign_ins(&ctx.db, &*user).await?;     // for a settings page
//! session.revoke(&ctx.db, &*user, id).await?;                  // "sign out that device"
//! kiln::auth::sign_out_everywhere(&ctx.db, &user).await?;      // after a password change
//! ```
//!
//! Sign-ins unused for 30 days expire.
//!
//! **Passwords.** [`hash_password`] and [`verify_password`] use Argon2id.
//!
//! **Tokens.** [`Tokens`] makes signed, expiring, single-purpose tokens for links in email
//! (password resets, confirmations). Bind a token to state that changes once it's used,
//! such as the password digest, and it stops working after one use.

use std::time::Duration;

use argon2::{
    Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{Db, Error, Id, Record, Result, SecretKey, Session, Timestamp, Value};

/// Sign-ins unused this long expire.
const IDLE_EXPIRY: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// `last_seen_at` is written at most this often per sign-in.
const TOUCH_EVERY: Duration = Duration::from_secs(60);

/// One place someone is signed in, for a "your sessions" page.
#[derive(Debug, Clone, Serialize)]
pub struct SignIn {
    pub id: i64,
    /// "Firefox on macOS", from the user agent.
    pub device: String,
    pub user_agent: String,
    pub ip: String,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    /// The sign-in this request is using.
    pub current: bool,
}

fn now() -> i64 {
    Timestamp::now().as_jiff().as_second()
}

fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl Db {
    pub(crate) async fn with_session_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_sessions\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \
             \"token_digest\" TEXT NOT NULL UNIQUE, \"record_table\" TEXT NOT NULL, \"record_id\" INTEGER NOT NULL, \
             \"user_agent\" TEXT NOT NULL, \"ip\" TEXT NOT NULL, \"created_at\" INTEGER NOT NULL, \"last_seen_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS \"kiln_sessions_record\" ON \"kiln_sessions\" (\"record_table\", \"record_id\")",
        )
        .await?;
        Ok(self)
    }
}

/// Stores a sign-in and returns its token (for the cookie).
pub(crate) async fn create_sign_in(db: &Db, table: &str, record_id: i64, user_agent: &str, ip: &str) -> Result<String> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let now = now();
    db.fetch_all(
        "INSERT INTO \"kiln_sessions\" (\"token_digest\", \"record_table\", \"record_id\", \"user_agent\", \"ip\", \"created_at\", \"last_seen_at\") \
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING \"id\"",
        vec![
            Value::Text(digest(&token)),
            Value::Text(table.to_owned()),
            Value::Integer(record_id),
            Value::Text(user_agent.chars().take(500).collect()),
            Value::Text(ip.to_owned()),
            Value::Integer(now),
            Value::Integer(now),
        ],
    )
    .await?;
    if bytes[0] == 0 {
        // Now and then, drop expired sign-ins.
        let cutoff = now - IDLE_EXPIRY.as_secs() as i64;
        db.fetch_all("DELETE FROM \"kiln_sessions\" WHERE \"last_seen_at\" < ? RETURNING \"id\"", vec![Value::Integer(cutoff)])
            .await?;
    }
    Ok(token)
}

/// The record a live sign-in token belongs to, in one query.
pub(crate) async fn find_signed_in<T: Record>(db: &Db, token: &str) -> Result<Option<T>> {
    let live = crate::Expr::<T>::raw(
        "\"id\" = (SELECT \"record_id\" FROM \"kiln_sessions\" WHERE \"token_digest\" = ? AND \"record_table\" = ? AND \"last_seen_at\" >= ?)",
        vec![
            Value::Text(digest(token)),
            Value::Text(T::TABLE.to_owned()),
            Value::Integer(now() - IDLE_EXPIRY.as_secs() as i64),
        ],
    );
    T::query().filter(live).one(db).await
}

/// Records that the sign-in was used, if the session says it's been a minute. The time of
/// the last write is kept in the session so most requests skip it.
pub(crate) async fn touch_sign_in(db: &Db, session: &Session, table: &str, token: &str) -> Result<()> {
    let key = format!("current.{table}.seen");
    let now = now();
    if session.get::<i64>(&key).is_some_and(|seen| now - seen < TOUCH_EVERY.as_secs() as i64) {
        return Ok(());
    }
    db.fetch_all(
        "UPDATE \"kiln_sessions\" SET \"last_seen_at\" = ? WHERE \"token_digest\" = ? AND \"record_table\" = ? RETURNING \"id\"",
        vec![Value::Integer(now), Value::Text(digest(token)), Value::Text(table.to_owned())],
    )
    .await?;
    session.insert(&key, now);
    Ok(())
}

pub(crate) async fn delete_sign_in(db: &Db, table: &str, token: &str) -> Result<()> {
    db.fetch_all(
        "DELETE FROM \"kiln_sessions\" WHERE \"token_digest\" = ? AND \"record_table\" = ? RETURNING \"id\"",
        vec![Value::Text(digest(token)), Value::Text(table.to_owned())],
    )
    .await?;
    Ok(())
}

/// Everywhere `record` is signed in, most recently used first (none marked `current`).
pub async fn sign_ins_of<T: Record>(db: &Db, record: &T) -> Result<Vec<SignIn>> {
    list_sign_ins(db, record, None).await
}

async fn list_sign_ins<T: Record>(db: &Db, record: &T, current: Option<&str>) -> Result<Vec<SignIn>> {
    let rows = db
        .fetch_all(
            "SELECT \"id\", \"token_digest\", \"user_agent\", \"ip\", \"created_at\", \"last_seen_at\" FROM \"kiln_sessions\" \
             WHERE \"record_table\" = ? AND \"record_id\" = ? AND \"last_seen_at\" >= ? ORDER BY \"last_seen_at\" DESC, \"id\" DESC",
            vec![
                Value::Text(T::TABLE.to_owned()),
                Value::Integer(record.id().get()),
                Value::Integer(now() - IDLE_EXPIRY.as_secs() as i64),
            ],
        )
        .await?;
    let at = |s: i64| Timestamp::from_jiff(jiff::Timestamp::from_second(s).unwrap_or_default());
    Ok(rows
        .iter()
        .map(|r| {
            let user_agent: String = r.get(2);
            SignIn {
                id: r.get(0),
                current: current == Some(r.get::<&str, _>(1)),
                device: device(&user_agent),
                user_agent,
                ip: r.get(3),
                created_at: at(r.get(4)),
                last_seen_at: at(r.get(5)),
            }
        })
        .collect())
}

/// Signs `record` out on every device. Call it when the password changes.
pub async fn sign_out_everywhere<T: Record>(db: &Db, record: &T) -> Result<usize> {
    let rows = db
        .fetch_all(
            "DELETE FROM \"kiln_sessions\" WHERE \"record_table\" = ? AND \"record_id\" = ? RETURNING \"id\"",
            vec![Value::Text(T::TABLE.to_owned()), Value::Integer(record.id().get())],
        )
        .await?;
    Ok(rows.len())
}

impl Session {
    /// Signs `record` in as the current `T` (usually `User`): stores a sign-in row and
    /// rotates the CSRF token. Replaces any sign-in this browser already had as a `T`.
    pub async fn sign_in<T: Record>(&self, db: &Db, record: &T) -> Result<()> {
        if let Some(old) = self.sign_in_token::<T>() {
            delete_sign_in(db, T::TABLE, &old).await?;
        }
        let (user_agent, ip) = self.client();
        let token = create_sign_in(db, T::TABLE, record.id().get(), &user_agent, &ip).await?;
        self.set_sign_in_token::<T>(Some(token));
        self.insert(&format!("current.{}.seen", T::TABLE), now());
        Ok(())
    }

    /// Signs this browser out (other devices stay signed in).
    pub async fn sign_out<T: Record>(&self, db: &Db) -> Result<()> {
        if let Some(token) = self.sign_in_token::<T>() {
            delete_sign_in(db, T::TABLE, &token).await?;
        }
        self.set_sign_in_token::<T>(None);
        Ok(())
    }

    /// Everywhere `record` is signed in, most recently used first, with this browser's
    /// sign-in marked `current`.
    pub async fn sign_ins<T: Record>(&self, db: &Db, record: &T) -> Result<Vec<SignIn>> {
        let current = self.sign_in_token::<T>().map(|t| digest(&t));
        list_sign_ins(db, record, current.as_deref()).await
    }

    /// Signs out one of `record`'s sign-ins by id. Returns whether there was one; ids
    /// belonging to anyone else are ignored.
    pub async fn revoke<T: Record>(&self, db: &Db, record: &T, sign_in: i64) -> Result<bool> {
        let rows = db
            .fetch_all(
                "DELETE FROM \"kiln_sessions\" WHERE \"id\" = ? AND \"record_table\" = ? AND \"record_id\" = ? RETURNING \"token_digest\"",
                vec![Value::Integer(sign_in), Value::Text(T::TABLE.to_owned()), Value::Integer(record.id().get())],
            )
            .await?;
        let current = self.sign_in_token::<T>().map(|t| digest(&t));
        if rows.first().is_some_and(|r| current.as_deref() == Some(r.get::<&str, _>(0))) {
            self.set_sign_in_token::<T>(None);
        }
        Ok(!rows.is_empty())
    }

    /// Signs `record` out everywhere except this browser.
    pub async fn sign_out_others<T: Record>(&self, db: &Db, record: &T) -> Result<usize> {
        let keep = self.sign_in_token::<T>().map(|t| digest(&t)).unwrap_or_default();
        let rows = db
            .fetch_all(
                "DELETE FROM \"kiln_sessions\" WHERE \"record_table\" = ? AND \"record_id\" = ? AND \"token_digest\" != ? RETURNING \"id\"",
                vec![Value::Text(T::TABLE.to_owned()), Value::Integer(record.id().get()), Value::Text(keep)],
            )
            .await?;
        Ok(rows.len())
    }
}

/// "Firefox on macOS" from a user agent; good enough for a sessions list.
fn device(user_agent: &str) -> String {
    let ua = user_agent;
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Chrome/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else if ua.contains("curl/") {
        "curl"
    } else {
        "Unknown browser"
    };
    let os = if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        return browser.to_owned();
    };
    format!("{browser} on {os}")
}

/// Argon2id hashing. Tests use cheap parameters so suites stay fast; the hash records its
/// parameters, so verification works either way.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::Integrity(format!("hashing the password failed: {e}")))
}

/// Whether `password` matches `digest` (from [`hash_password`]). An empty or malformed
/// digest never matches.
pub fn verify_password(password: &str, digest: &str) -> bool {
    PasswordHash::new(digest).is_ok_and(|hash| Argon2::default().verify_password(password.as_bytes(), &hash).is_ok())
}

/// Checks a sign-in attempt. Pass `None` when no account has the email: a stand-in hash is
/// checked anyway, so response times don't reveal which emails have accounts.
pub fn check_password(password: &str, digest: Option<&str>) -> bool {
    static STAND_IN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    match digest {
        Some(digest) => verify_password(password, digest),
        None => {
            let stand_in = STAND_IN.get_or_init(|| hash_password("kiln stand-in").unwrap_or_default());
            let _ = verify_password(password, stand_in);
            false
        }
    }
}

fn argon2() -> Argon2<'static> {
    if crate::config::env() == "test" {
        let params = Params::new(256, 1, 1, None).expect("valid test params");
        Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
    } else {
        Argon2::default()
    }
}

/// Keeps email tokens apart from other signed strings (stream names).
const TOKEN_PREFIX: &str = "kiln-token/";

/// Signed, expiring tokens for links in email: `ctx.tokens().make("reset", user.id, ..)`.
#[derive(Clone)]
pub struct Tokens {
    key: SecretKey,
}

impl Tokens {
    pub(crate) fn new(key: SecretKey) -> Self {
        Self { key }
    }

    /// A token for `purpose` and `record`, valid for `ttl`. `bound_to` is state that
    /// changes when the token is used (a password digest, a confirmation time), which makes
    /// the token single-use.
    pub fn make<T: Record>(&self, purpose: &str, record: Id<T>, bound_to: &str, ttl: jiff::SignedDuration) -> String {
        let expires = (Timestamp::now() + ttl).as_jiff().as_second();
        let fingerprint = &digest(bound_to)[..16];
        self.key.sign_str(&format!("{TOKEN_PREFIX}{purpose}:{}:{}:{expires}:{fingerprint}", T::TABLE, record.get()))
    }

    /// The record id in a token made for `purpose`, if it's genuine and unexpired. Check
    /// it against the record with [`Tokens::matches`] once loaded.
    pub fn read<T: Record>(&self, purpose: &str, token: &str) -> Option<Id<T>> {
        let payload = self.key.verify_str(token)?;
        let mut parts = payload.strip_prefix(TOKEN_PREFIX)?.split(':');
        let (p, table, id, expires) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if p != purpose || table != T::TABLE || expires.parse::<i64>().ok()? < now() {
            return None;
        }
        id.parse().ok().map(Id::new)
    }

    /// Whether the token was made with this `bound_to` (it's unused).
    pub fn matches(&self, token: &str, bound_to: &str) -> bool {
        let fingerprint = &digest(bound_to)[..16];
        self.key.verify_str(token).is_some_and(|p| p.rsplit(':').next() == Some(fingerprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_verify() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn devices_are_named() {
        let firefox = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.0; rv:131.0) Gecko/20100101 Firefox/131.0";
        assert_eq!(device(firefox), "Firefox on macOS");
        assert_eq!(device("curl/8.4"), "curl");
    }
}
