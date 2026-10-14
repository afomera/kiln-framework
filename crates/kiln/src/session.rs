//! Sessions, flash messages, CSRF tokens and the signed-in user.
//!
//! The session is a JSON map in a signed cookie (HMAC-SHA256). Kiln's request middleware
//! loads it before the action runs and writes it back only when it changed. Sign-ins are
//! rows in the database; the cookie holds only their token (see `kiln::auth`).

use std::{
    collections::BTreeMap,
    ops::Deref,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::request::Parts,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::{Ctx, Error, Record};

pub const COOKIE: &str = "_kiln_session";
const FLASH_KEY: &str = "_flash";
const CSRF_KEY: &str = "_csrf";

/// Key used to sign session cookies.
#[derive(Clone)]
pub struct SecretKey(Arc<[u8]>);

impl SecretKey {
    /// At least 32 bytes.
    pub fn new(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= 32, "a secret key needs at least 32 bytes");
        Self(bytes.into())
    }

    pub fn generate() -> Self {
        let mut bytes = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes.as_slice().into())
    }

    fn mac(&self, payload: &[u8]) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(payload);
        mac.finalize().into_bytes().to_vec()
    }

    pub(crate) fn sign(&self, data: &Map<String, Value>) -> String {
        let payload = serde_json::to_vec(data).expect("session serializes");
        format!("{}.{}", URL_SAFE_NO_PAD.encode(&payload), URL_SAFE_NO_PAD.encode(self.mac(&payload)))
    }

    /// `name.signature`, for values handed to the browser that it must not forge.
    pub(crate) fn sign_str(&self, value: &str) -> String {
        format!("{}.{}", URL_SAFE_NO_PAD.encode(value), URL_SAFE_NO_PAD.encode(self.mac(value.as_bytes())))
    }

    pub(crate) fn verify_str(&self, signed: &str) -> Option<String> {
        let (value, signature) = signed.split_once('.')?;
        let value = URL_SAFE_NO_PAD.decode(value).ok()?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).ok()?;
        mac.update(&value);
        mac.verify_slice(&signature).ok()?;
        String::from_utf8(value).ok()
    }

    pub(crate) fn verify(&self, cookie: &str) -> Option<Map<String, Value>> {
        let (payload, signature) = cookie.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).ok()?;
        mac.update(&payload);
        mac.verify_slice(&signature).ok()?;
        serde_json::from_slice(&payload).ok()
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

/// The current request's session. Extract it in an action, or read it through
/// [`Current`] and the generated helpers.
#[derive(Clone, Default)]
pub struct Session {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    data: Map<String, Value>,
    changed: bool,
    /// `(user agent, ip)` of the request, recorded with sign-ins.
    client: (String, String),
}

impl Session {
    pub(crate) fn from_data(data: Map<String, Value>) -> Self {
        Self { inner: Arc::new(Mutex::new(State { data, changed: false, client: Default::default() })) }
    }

    pub(crate) fn set_client(&self, user_agent: String, ip: String) {
        self.state().client = (user_agent, ip);
    }

    pub(crate) fn client(&self) -> (String, String) {
        self.state().client.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state().data.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn insert<T: Serialize>(&self, key: &str, value: T) {
        let value = serde_json::to_value(value).expect("session values serialize");
        let mut state = self.state();
        state.data.insert(key.to_owned(), value);
        state.changed = true;
    }

    pub fn remove(&self, key: &str) {
        let mut state = self.state();
        if state.data.remove(key).is_some() {
            state.changed = true;
        }
    }

    /// Clears everything, including the sign-in. A new CSRF token is issued on next use.
    pub fn clear(&self) {
        let mut state = self.state();
        state.data.clear();
        state.changed = true;
    }

    /// The sign-in token for `T` (see `kiln::auth`).
    pub(crate) fn sign_in_token<T: Record>(&self) -> Option<String> {
        self.get::<String>(&current_key::<T>())
    }

    /// Sets or clears the sign-in token for `T`, rotating the CSRF token either way.
    pub(crate) fn set_sign_in_token<T: Record>(&self, token: Option<String>) {
        match token {
            Some(token) => self.insert(&current_key::<T>(), token),
            None => {
                self.remove(&current_key::<T>());
                self.remove(&format!("{}.seen", current_key::<T>()));
            }
        }
        self.remove(CSRF_KEY);
    }

    /// The CSRF token for this session, created on first use.
    pub fn csrf_token(&self) -> String {
        if let Some(token) = self.get::<String>(CSRF_KEY) {
            return token;
        }
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        self.insert(CSRF_KEY, &token);
        token
    }

    pub(crate) fn existing_csrf_token(&self) -> Option<String> {
        self.get(CSRF_KEY)
    }

    /// Takes the flash set by the previous request.
    pub(crate) fn take_flash(&self) -> Flash {
        let flash = self.get::<BTreeMap<String, String>>(FLASH_KEY).unwrap_or_default();
        self.remove(FLASH_KEY);
        Flash(flash)
    }

    pub(crate) fn set_flash(&self, flash: &Flash) {
        if !flash.0.is_empty() {
            self.insert(FLASH_KEY, &flash.0);
        }
    }

    pub(crate) fn changed(&self) -> bool {
        self.state().changed
    }

    pub(crate) fn data(&self) -> Map<String, Value> {
        self.state().data.clone()
    }
}

fn current_key<T: Record>() -> String {
    format!("current.{}", T::TABLE)
}

impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or_else(|| Error::Integrity("no session: the request didn't go through kiln's App router".into()))
    }
}

/// Messages for the next page, e.g. `notice` → "Post created". Templates see `flash`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Flash(pub BTreeMap<String, String>);

/// The signed-in record of type `T`, usually `Current<User>`. Derefs to `T`.
///
/// Requests without a signed-in `T` get `Error::Unauthenticated` (401, or a redirect to
/// the sign-in page for browsers when the app sets one). Use `Option<Current<T>>` for
/// pages that work signed out.
#[derive(Debug, Clone)]
pub struct Current<T>(pub T);

impl<T> Deref for Current<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> Current<T> {
    pub fn into_inner(self) -> T {
        self.0
    }

    /// The API token this request was authenticated with, or none for a cookie sign-in.
    pub fn api_token(&self) -> Option<crate::auth::ApiToken> {
        current_scope().and_then(|scope| scope.cache.api_token.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Checks that the request may act with `scope` (e.g. `"repo:write"`): API tokens need it
    /// among their scopes; cookie sign-ins have every scope. Otherwise a 403 (K1021) naming
    /// the missing scope.
    pub fn require_scope(&self, scope: &str) -> Result<(), Error> {
        let Some(request) = current_scope() else {
            return Err(Error::Integrity(format!(
                "`require_scope(\"{scope}\")` was called outside a request; call it in the action, before spawning work"
            )));
        };
        let token = request.cache.api_token.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match token {
            Some(token) if !token.has_scope(scope) => Err(Error::MissingScope { scope: scope.to_owned() }),
            _ => Ok(()),
        }
    }
}

async fn load_current<T: Record>(parts: &Parts, ctx: &Ctx) -> Result<Option<T>, Error> {
    let Some(session) = parts.extensions.get::<Session>() else { return Ok(None) };
    signed_in::<T>(session, &ctx.db).await
}

/// The signed-in `T`, loaded at most once per request: the action's `Current<T>` and the
/// layout's `current_*` value share one query.
pub(crate) async fn signed_in<T: Record>(session: &Session, db: &crate::Db) -> Result<Option<T>, Error> {
    let scope = current_scope();
    let cache = scope.as_ref().map(|scope| scope.cache.clone());
    if let Some(hit) = cache.as_ref().and_then(|c| c.get::<T>()) {
        return Ok(hit);
    }
    let presented = scope.and_then(|scope| scope.api_token);
    let record = match (presented, session.sign_in_token::<T>()) {
        // API routes: the token, never a cookie (their session is always empty).
        (Some(digest), _) => match crate::auth::find_by_api_token::<T>(db, &digest).await? {
            Some((record, token)) => {
                if let Some(cache) = &cache {
                    *cache.api_token.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
                }
                Some(record)
            }
            None => None,
        },
        (None, Some(token)) => {
            let record = crate::auth::find_signed_in::<T>(db, &token).await?;
            if record.is_some() {
                crate::auth::touch_sign_in(db, session, T::TABLE, &token).await?;
            } else {
                // Signed out elsewhere, expired, or the record was deleted.
                session.set_sign_in_token::<T>(None);
            }
            record
        }
        (None, None) => None,
    };
    if let Some(cache) = cache {
        cache.put(record.clone());
    }
    Ok(record)
}

impl<T: Record> FromRequestParts<Ctx> for Current<T> {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Self::Rejection> {
        load_current::<T>(parts, ctx).await?.map(Current).ok_or(Error::Unauthenticated { record: T::TABLE })
    }
}

impl<T: Record> OptionalFromRequestParts<Ctx> for Current<T> {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Option<Self>, Self::Rejection> {
        Ok(load_current::<T>(parts, ctx).await?.map(Current))
    }
}

/// Per-request values that templates can read: flash, CSRF token, `current_*`. Set by the
/// middleware for the duration of the action and the render that follows it.
#[derive(Clone, Default)]
pub(crate) struct RequestScope {
    pub session: Session,
    pub flash: Flash,
    pub cache: Arc<RequestCache>,
    /// A Turbo Frame request: pages render without their layout.
    pub frame: bool,
    /// Signs stream names for `turbo_stream_from`.
    pub key: Option<SecretKey>,
    /// On API routes, the SHA-256 of the API token the request carries.
    pub api_token: Option<String>,
}

/// Records loaded for this request, by table, and the `current_*` template values.
#[derive(Default)]
pub(crate) struct RequestCache {
    records: Mutex<std::collections::HashMap<&'static str, Option<Arc<dyn std::any::Any + Send + Sync>>>>,
    pub templates: Mutex<BTreeMap<String, minijinja::Value>>,
    /// The API token that authenticated this request, once `Current<T>` found it.
    pub api_token: Mutex<Option<crate::auth::ApiToken>>,
    /// `users:12` for the first signed-in record found, the key rate limits count under.
    pub signed_in_as: Mutex<Option<String>>,
}

impl RequestCache {
    /// `Some(found)` if this request already looked up the signed-in `T`.
    fn get<T: Record>(&self) -> Option<Option<T>> {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        records.get(T::TABLE).map(|hit| hit.as_ref().and_then(|any| any.downcast_ref::<T>().cloned()))
    }

    fn put<T: Record>(&self, record: Option<T>) {
        if let Some(record) = &record {
            let mut who = self.signed_in_as.lock().unwrap_or_else(|e| e.into_inner());
            who.get_or_insert_with(|| format!("{}:{}", T::TABLE, record.id().get()));
        }
        let value = record.map(|r| Arc::new(r) as Arc<dyn std::any::Any + Send + Sync>);
        self.records.lock().unwrap_or_else(|e| e.into_inner()).insert(T::TABLE, value);
    }

    pub fn template_values(&self) -> BTreeMap<String, minijinja::Value> {
        self.templates.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

tokio::task_local! {
    pub(crate) static REQUEST: RequestScope;
}

pub(crate) fn current_scope() -> Option<RequestScope> {
    REQUEST.try_with(Clone::clone).ok()
}

/// Constant-time string comparison for tokens.
pub(crate) fn tokens_match(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
