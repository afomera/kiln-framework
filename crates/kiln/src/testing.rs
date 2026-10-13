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
        Self::configured(migrations, Ok).await
    }

    /// A test app with the app's own settings applied (`#[kiln::test]` passes the app's
    /// `app::configure` when `src/app.rs` exists).
    pub async fn configured(migrations: &Path, configure: impl FnOnce(App) -> Result<App>) -> Result<Self> {
        crate::config::use_test_env();
        let db = Db::memory().await?;
        db.migrate(migrations).await?;
        // `db/migrations` lives in the app's root, next to `assets/` and `public/`.
        let root = migrations.parent().and_then(Path::parent).unwrap_or(Path::new("."));
        crate::config::set_root(root);
        Ok(Self { app: configure(App::new(db.clone()).root(root))?, db, sequence: AtomicU64::new(0) })
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
        // The sign-in row is stored on the client's first request (this stays synchronous).
        *client.pending_sign_in.lock().unwrap_or_else(|e| e.into_inner()) = Some((U::TABLE, record.id().get()));
        client
    }

    /// Signed-out GET, as an agent or script would send it.
    pub async fn get(&self, path: impl AsRef<str>) -> TestResponse {
        self.client().get(path).await
    }

    /// Signed-out GET, as a browser sends it (`Accept: text/html`).
    pub async fn get_html(&self, path: impl AsRef<str>) -> TestResponse {
        self.client().get_html(path).await
    }

    /// Runs every queued job (and any jobs they enqueue) now, ignoring delays. Returns how
    /// many runs succeeded. Failures retry as usual; a job that uses up its attempts makes
    /// this an error naming the job.
    pub async fn perform_enqueued_jobs(&self) -> Result<usize> {
        crate::jobs::perform_all(self.app.ctx()).await
    }

    /// Queued jobs of type `J`, oldest first.
    pub async fn enqueued<J: crate::jobs::JobInfo>(&self) -> Result<Vec<J>> {
        self.db
            .queued_payloads(J::NAME)
            .await?
            .iter()
            .map(|p| serde_json::from_str(p).map_err(|e| crate::Error::BadRequest(e.to_string())))
            .collect()
    }

    /// Asserts exactly `count` jobs of type `J` are queued.
    #[track_caller]
    pub fn assert_enqueued<J: crate::jobs::JobInfo>(&self, count: usize) -> impl Future<Output = Result> + '_ {
        let caller = std::panic::Location::caller();
        async move {
            let queued = self.db.queued_payloads(J::NAME).await?;
            assert_eq!(queued.len(), count, "{caller}: expected {count} queued `{}` job(s); queued: {queued:#?}", J::NAME);
            Ok(())
        }
    }

    /// Mail delivered so far, oldest first. `deliver_later` queues a job: run
    /// `app.perform_enqueued_jobs()` first.
    pub async fn outbox(&self) -> Result<Vec<crate::mail::Message>> {
        Ok(self.db.outbox().await?.into_iter().map(|(_, m)| m).collect())
    }

    /// Asserts mail to `to` was delivered with `needle` in its subject or body. Returns it.
    #[track_caller]
    pub fn assert_mailed<'a>(&'a self, to: &'a str, needle: &'a str) -> impl Future<Output = Result<crate::mail::Message>> + 'a {
        let caller = std::panic::Location::caller();
        async move {
            let outbox = self.outbox().await?;
            if let Some(found) = outbox.iter().find(|m| m.to == to && m.contains(needle)) {
                return Ok(found.clone());
            }
            let queued = self.db.queued_payloads("Deliver").await?.len();
            let hint = if queued > 0 {
                format!(" ({queued} queued; run `app.perform_enqueued_jobs().await?` first)")
            } else {
                String::new()
            };
            panic!("{caller}: expected mail to {to} containing {needle:?}{hint}; the outbox has {outbox:#?}")
        }
    }

    /// Payloads broadcast on `stream` so far, oldest first.
    pub async fn broadcasts(&self, stream: &str) -> Result<Vec<String>> {
        Ok(self.db.stream_messages(stream, 0).await?.into_iter().map(|(_, payload)| payload).collect())
    }

    /// Asserts something broadcast on `stream` contains `needle`.
    #[track_caller]
    pub fn assert_broadcast<'a>(&'a self, stream: &'a str, needle: &'a str) -> impl Future<Output = Result> + 'a {
        let caller = std::panic::Location::caller();
        async move {
            let sent = self.broadcasts(stream).await?;
            assert!(
                sent.iter().any(|p| p.contains(needle)),
                "{caller}: expected a broadcast on `{stream}` containing {needle:?}; broadcasts were {sent:#?}"
            );
            Ok(())
        }
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
    db: Db,
    session: Mutex<Map<String, Value>>,
    pending_sign_in: Mutex<Option<(&'static str, i64)>>,
}

const TEST_CSRF: &str = "test-csrf-token";

impl TestClient {
    pub fn new(app: &App) -> Self {
        Self {
            router: app.router(),
            key: app.key().clone(),
            db: app.ctx().db.clone(),
            session: Mutex::default(),
            pending_sign_in: Mutex::default(),
        }
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

    /// Submits a form the way Turbo does: accepting `text/vnd.turbo-stream.html`.
    pub async fn turbo_post_form(&self, path: impl AsRef<str>, fields: &[(&str, &str)]) -> TestResponse {
        self.form_with_accept(Method::POST, path.as_ref(), fields, Some("text/vnd.turbo-stream.html, text/html")).await
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

    /// Opens a Server-Sent Events stream (e.g. a page's `<turbo-stream-source>` URL) and
    /// reads it until `until` matches what's arrived, or two seconds pass. Returns the text
    /// received. `last_event_id` resumes like a reconnecting browser.
    pub async fn read_stream(&self, path: &str, last_event_id: Option<&str>, until: impl Fn(&str) -> bool) -> String {
        use futures_util::StreamExt;
        let mut builder = Request::builder().method(Method::GET).uri(path);
        if let Some(id) = last_event_id {
            builder = builder.header("last-event-id", id);
        }
        let request = builder.body(Body::empty()).expect("valid test request");
        let response = self.router.clone().oneshot(request).await.expect("router is infallible");
        let mut body = response.into_body().into_data_stream();
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !until(&text) {
            match tokio::time::timeout_at(deadline, body.next()).await {
                Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
                _ => break,
            }
        }
        text
    }

    /// Any method, optionally with an `Accept` header.
    pub async fn request(&self, method: Method, path: &str, accept: Option<&str>) -> TestResponse {
        self.send(method, path, accept, None).await
    }

    async fn form_with_accept(
        &self,
        method: Method,
        path: &str,
        fields: &[(&str, &str)],
        accept: Option<&str>,
    ) -> TestResponse {
        let (content_type, body) = self.form_body(fields, true);
        self.send(method, path, accept, Some((content_type, body))).await
    }

    /// The urlencoded body for `fields`, with the session's CSRF token when `csrf`.
    fn form_body(&self, fields: &[(&str, &str)], csrf: bool) -> (&'static str, String) {
        let mut pairs: Vec<(&str, &str)> = fields.to_vec();
        let token = csrf.then(|| {
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let token = session.entry("_csrf").or_insert(Value::from(TEST_CSRF));
            token.as_str().unwrap_or(TEST_CSRF).to_owned()
        });
        if let Some(token) = &token {
            pairs.push(("_csrf", token));
        }
        ("application/x-www-form-urlencoded", serde_urlencoded::to_string(&pairs).expect("form fields encode"))
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
        let pending = self.pending_sign_in.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((table, id)) = pending {
            let token = crate::auth::create_sign_in(&self.db, table, id, "kiln test client", "127.0.0.1")
                .await
                .expect("storing the test sign-in");
            self.session_insert(&format!("current.{table}"), Value::from(token));
            self.session_insert(&format!("current.{table}.seen"), Value::from(jiff::Timestamp::now().as_second()));
        }
        let mut builder = Request::builder().method(method).uri(path).header(header::USER_AGENT, "kiln test client");
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
