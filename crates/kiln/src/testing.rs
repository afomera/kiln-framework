//! Test harness: a fresh in-memory app per test, factories, and an in-process HTTP client
//! that carries a session (signed-in user, CSRF token, flash).
//!
//! ```ignore
//! #[kiln::test]
//! async fn author_can_publish(app: TestApp) -> kiln::Result {
//!     let author = app.create::<User>().await?;
//!     app.sign_in_as(&author).post_form(posts::create::path(), &[("title", "Hello")]).await
//!         .assert_status(303)
//!         .assert_flash("Post created.");
//!     Ok(())
//! }
//! ```

use std::{
    collections::BTreeMap,
    future::Future,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use serde_json::{Map, Value};
use tower::ServiceExt;

use crate::{App, Db, Record, Result, SecretKey, session::COOKIE};

/// `json!({ "email": ".." })` for API tests, without a `serde_json` dependency.
pub use serde_json::json;

/// A fresh app on a private in-memory database with migrations applied.
pub struct TestApp {
    pub db: Db,
    app: App,
    sequence: AtomicU64,
}

impl TestApp {
    pub async fn new(migrations: &Path) -> Result<Self> {
        Self::configured(migrations, std::convert::identity).await
    }

    /// A test app with the app's own settings applied (`#[kiln::test]` passes the app's
    /// `app::configure` when `src/app.rs` exists).
    pub async fn configured(migrations: &Path, configure: impl FnOnce(App) -> App) -> Result<Self> {
        let db = Db::memory().await?;
        db.migrate(migrations).await?;
        Ok(Self { app: configure(App::new(db.clone())), db, sequence: AtomicU64::new(0) })
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// A signed-out client.
    pub fn client(&self) -> TestClient {
        TestClient::new(&self.app)
    }

    /// A client signed in as `record` (a `User`, `Merchant`, ... anything that signs in).
    pub fn sign_in_as<U: Record>(&self, record: &U) -> TestClient {
        let client = self.client();
        client.session_insert(&format!("current.{}", U::TABLE), Value::from(record.id().get()));
        client
    }

    /// Signed-out GET, as an agent or script would send it.
    pub async fn get(&self, path: impl AsRef<str>) -> TestResponse {
        self.client().get(path).await
    }

    /// A unique number per call, for unique factory values.
    pub fn sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Inserts a record built by its [`Factory`].
    pub async fn create<T: Factory>(&self) -> Result<T> {
        self.create_with::<T>(|_| {}).await
    }

    /// Inserts a record built by its [`Factory`], changed by `customize` first.
    pub async fn create_with<T: Factory>(&self, customize: impl FnOnce(&mut T::New)) -> Result<T> {
        let mut new = T::build(self, self.sequence()).await?;
        customize(&mut new);
        T::insert(new).run(&self.db).await
    }
}

/// Default values for creating a record in tests. Lives in `src/factories.rs` so both the
/// app and its integration tests can use it.
pub trait Factory: Record {
    /// Builds the insert for the `n`th record of this type. Create parents through `app`.
    fn build(app: &TestApp, n: u64) -> impl Future<Output = Result<Self::New>> + Send;
}

/// In-process HTTP client. Requests go through the real router and middleware.
pub struct TestClient {
    router: axum::Router,
    key: SecretKey,
    session: Mutex<Map<String, Value>>,
}

const TEST_CSRF: &str = "test-csrf-token";

impl TestClient {
    pub fn new(app: &App) -> Self {
        Self { router: app.router(), key: app.key().clone(), session: Mutex::default() }
    }

    fn session_insert(&self, key: &str, value: Value) {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_owned(), value);
    }

    pub async fn get(&self, path: impl AsRef<str>) -> TestResponse {
        self.send(Method::GET, path.as_ref(), None, None).await
    }

    /// Like [`TestClient::get`], but as a browser: `Accept: text/html`.
    pub async fn get_html(&self, path: impl AsRef<str>) -> TestResponse {
        self.send(Method::GET, path.as_ref(), Some("text/html,application/xhtml+xml,*/*;q=0.8"), None).await
    }

    /// Submits a form the way a browser does, CSRF token included.
    pub async fn post_form(&self, path: impl AsRef<str>, fields: &[(&str, &str)]) -> TestResponse {
        self.form(Method::POST, path.as_ref(), fields, true).await
    }

    pub async fn patch_form(&self, path: impl AsRef<str>, fields: &[(&str, &str)]) -> TestResponse {
        self.form(Method::PATCH, path.as_ref(), fields, true).await
    }

    pub async fn put_form(&self, path: impl AsRef<str>, fields: &[(&str, &str)]) -> TestResponse {
        self.form(Method::PUT, path.as_ref(), fields, true).await
    }

    pub async fn delete(&self, path: impl AsRef<str>) -> TestResponse {
        self.form(Method::DELETE, path.as_ref(), &[], true).await
    }

    /// A form submission without a CSRF token, for testing that it's rejected.
    pub async fn post_form_without_csrf(&self, path: impl AsRef<str>, fields: &[(&str, &str)]) -> TestResponse {
        self.form(Method::POST, path.as_ref(), fields, false).await
    }

    /// Any method, optionally with an `Accept` header.
    pub async fn request(&self, method: Method, path: &str, accept: Option<&str>) -> TestResponse {
        self.send(method, path, accept, None).await
    }

    async fn form(&self, method: Method, path: &str, fields: &[(&str, &str)], csrf: bool) -> TestResponse {
        let mut pairs: Vec<(&str, &str)> = fields.to_vec();
        // Like a browser, submit the token the session holds (issued by an earlier page).
        let token = csrf.then(|| {
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let token = session.entry("_csrf").or_insert(Value::from(TEST_CSRF));
            token.as_str().unwrap_or(TEST_CSRF).to_owned()
        });
        if let Some(token) = &token {
            pairs.push(("_csrf", token));
        }
        let body = serde_urlencoded::to_string(&pairs).expect("form fields encode");
        self.send(method, path, None, Some(("application/x-www-form-urlencoded", body))).await
    }

    /// Sends a JSON body, as an API client does (no cookies or CSRF token needed).
    pub async fn post_json(&self, path: impl AsRef<str>, body: &serde_json::Value) -> TestResponse {
        self.send(Method::POST, path.as_ref(), None, Some(("application/json", body.to_string()))).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        accept: Option<&str>,
        body: Option<(&str, String)>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(accept) = accept {
            builder = builder.header(header::ACCEPT, accept);
        }
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !session.is_empty() {
            builder = builder.header(header::COOKIE, format!("{COOKIE}={}", self.key.sign(&session)));
        }
        let body = match body {
            Some((content_type, body)) => {
                builder = builder.header(header::CONTENT_TYPE, content_type);
                Body::from(body)
            }
            None => Body::empty(),
        };
        let request = builder.body(body).expect("valid test request");
        let response = self.router.clone().oneshot(request).await.expect("router is infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("readable body");

        let mut flash = BTreeMap::new();
        if let Some(updated) = self.read_session_cookie(&headers) {
            if let Some(Value::Object(f)) = updated.get("_flash") {
                flash = f.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect();
            }
            *self.session.lock().unwrap_or_else(|e| e.into_inner()) = updated;
        }
        TestResponse { status, headers, text: String::from_utf8_lossy(&bytes).into_owned(), flash }
    }

    /// The session from a `Set-Cookie` header, if the response changed it.
    fn read_session_cookie(&self, headers: &HeaderMap) -> Option<Map<String, Value>> {
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| v.strip_prefix(COOKIE)?.strip_prefix('='))?;
        let value = cookie.split(';').next().unwrap_or_default();
        if value.is_empty() { Some(Map::new()) } else { self.key.verify(value) }
    }
}

#[derive(Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
    /// Flash set by this response (for the next page), e.g. `notice`.
    pub flash: BTreeMap<String, String>,
}

impl TestResponse {
    #[track_caller]
    pub fn assert_status(&self, code: u16) -> &Self {
        assert_eq!(self.status.as_u16(), code, "unexpected status; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_contains(&self, needle: &str) -> &Self {
        assert!(self.text.contains(needle), "expected body to contain {needle:?}; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_not_contains(&self, needle: &str) -> &Self {
        assert!(!self.text.contains(needle), "expected body not to contain {needle:?}; body:\n{}", self.text);
        self
    }

    #[track_caller]
    pub fn assert_redirect(&self, to: &str) -> &Self {
        assert!(self.status.is_redirection(), "expected a redirect, got {}; body:\n{}", self.status, self.text);
        let location = self.headers.get(header::LOCATION).and_then(|v| v.to_str().ok());
        assert_eq!(location, Some(to), "unexpected redirect target");
        self
    }

    /// The body parsed as JSON.
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("body isn't JSON ({e}):\n{}", self.text))
    }

    /// Asserts any flash message equals `message`.
    #[track_caller]
    pub fn assert_flash(&self, message: &str) -> &Self {
        assert!(self.flash.values().any(|m| m == message), "expected flash {message:?}; flash was {:?}", self.flash);
        self
    }
}
