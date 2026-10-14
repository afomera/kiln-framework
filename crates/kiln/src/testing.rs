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
    http::{HeaderMap, Request, StatusCode, header},
};
use serde_json::{Map, Value};
use tower::ServiceExt;

use crate::{App, Ctx, Db, Record, Result, SecretKey, Timestamp, session::COOKIE};

/// `json!({ "email": ".." })` for API tests, without a `serde_json` dependency.
pub use serde_json::json;

/// For `client.request(Method::PUT, ..)`.
pub use axum::http::Method;

/// A file for `post_multipart` / `patch_multipart`.
#[derive(Debug, Clone)]
pub struct Upload {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl Upload {
    pub fn new(filename: &str, content_type: &str, bytes: impl Into<Vec<u8>>) -> Self {
        Self { filename: filename.to_owned(), content_type: content_type.to_owned(), bytes: bytes.into() }
    }

    /// A 1×1 PNG.
    pub fn png(filename: &str) -> Self {
        const PIXEL: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
            0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff,
            0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        Self::new(filename, "image/png", PIXEL)
    }

    /// A `width` × `height` PNG (a gradient), for testing image variants.
    pub fn png_sized(filename: &str, width: u32, height: u32) -> Self {
        let img = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).expect("PNG encodes");
        Self::new(filename, "image/png", bytes)
    }

    /// A text file.
    pub fn text(filename: &str, contents: &str) -> Self {
        Self::new(filename, "text/plain", contents.as_bytes())
    }
}

/// A fresh app on a private in-memory database with migrations applied.
pub struct TestApp {
    pub db: Db,
    app: App,
    sequence: AtomicU64,
    clock: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

impl TestApp {
    pub async fn new(migrations: &Path) -> Result<Self> {
        Self::configured(migrations, Ok).await
    }

    /// A test app with the app's own settings applied (`#[kiln::test]` passes the app's
    /// `app::configure` when `src/app.rs` exists).
    pub async fn configured(migrations: &Path, configure: impl FnOnce(App) -> Result<App>) -> Result<Self> {
        crate::config::use_test_env();
        // `db/migrations` lives in the app's root, next to `assets/` and `public/`.
        let root = migrations.parent().and_then(Path::parent).unwrap_or(Path::new("."));
        let mut db = test_database(root, migrations).await?;
        // `KILN_TEST_SPLIT_DATABASES=1`: the queue, cache and broadcasts each in a database of
        // their own, as an app with `QUEUE_DATABASE_URL` (and friends) runs.
        if std::env::var("KILN_TEST_SPLIT_DATABASES").as_deref() == Ok("1") && db.dialect().name() == "sqlite" {
            for role in [crate::db::Role::Queue, crate::db::Role::Cache, crate::db::Role::Streams] {
                db = db.with_role(role, Db::memory().await?);
            }
        }
        crate::config::set_root(root);
        // Files go to a store of the test's own, gone when it ends.
        // Webhooks are captured until a test answers them with `receive_webhooks`.
        let app = configure(
            App::new(db.clone())
                .root(root)
                .provide(crate::Storage::memory())
                .provide(crate::webhooks::TestReceiver::default()),
        )?;
        crate::jobs::sync_app(app.ctx()).await?;
        Ok(Self { app, db, sequence: AtomicU64::new(0), clock: Default::default() })
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// Runs `test` with this app's clock, so `travel` moves `Timestamp::now()` for it (and
    /// for the requests and jobs it runs). `#[kiln::test]` calls this.
    pub async fn run<F, Fut>(self, test: F) -> Fut::Output
    where
        F: FnOnce(Self) -> Fut,
        Fut: std::future::Future,
    {
        let clock = self.clock.clone();
        crate::types::CLOCK_OFFSET.scope(clock, test(self)).await
    }

    /// Moves the clock by `by` (negative goes back): `Timestamp::now()` in the test, its
    /// requests and the jobs it performs. Time keeps passing from there.
    pub fn travel(&self, by: jiff::SignedDuration) {
        self.clock.fetch_add(by.as_micros() as i64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Moves the clock to `at`.
    pub fn travel_to(&self, at: Timestamp) {
        let real = jiff::Timestamp::now();
        let offset = at.as_jiff().duration_since(real).as_micros() as i64;
        self.clock.store(offset, std::sync::atomic::Ordering::Relaxed);
    }

    /// Puts the clock back.
    pub fn travel_back(&self) {
        self.clock.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// A `Ctx` for calling ops and jobs directly: `publish_post(app.ctx(), ..)`.
    pub fn ctx(&self) -> &Ctx {
        self.app.ctx()
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

    /// Performs the jobs due by now (see `travel`), leaving later ones queued; repeats until
    /// none are left, so jobs they enqueue run too.
    pub async fn perform_due_jobs(&self) -> Result<usize> {
        crate::jobs::perform_due(self.app.ctx()).await
    }

    /// Enqueues the schedule entries due by now (see `travel`): `app.travel(..)`, then this.
    pub async fn run_schedules(&self) -> Result<Vec<String>> {
        self.run_schedules_at(Timestamp::now()).await
    }

    /// Runs the app's schedule as if the clock read `at`: enqueues every entry due by
    /// then (once each, however many slots passed). Returns the job names enqueued.
    pub async fn run_schedules_at(&self, at: Timestamp) -> Result<Vec<String>> {
        if let Some(schedule) = self.app.ctx().try_get::<crate::Schedule>() {
            self.db.sync_schedules(schedule, Timestamp::now()).await?;
        }
        self.db.run_due_schedules(at).await
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
            assert_eq!(
                queued.len(),
                count,
                "{caller}: expected {count} queued `{}` job(s); queued: {queued:#?}",
                J::NAME
            );
            Ok(())
        }
    }

    /// Notifications of type `N` sent and not yet delivered (each `notify` queues one
    /// delivery), oldest first, with the recipient's id. `perform_enqueued_jobs` delivers them.
    pub async fn notified<N: crate::notify::Notify>(&self) -> Result<Vec<(i64, N)>> {
        use crate::jobs::JobInfo;
        let mut out = vec![];
        for payload in self.db.queued_payloads(crate::notify::DeliverNotification::NAME).await? {
            let job: crate::notify::DeliverNotification =
                serde_json::from_str(&payload).map_err(|e| crate::Error::BadRequest(e.to_string()))?;
            if job.name == N::NAME && job.to_table == <N::To as Record>::TABLE {
                let notification =
                    serde_json::from_value(job.payload).map_err(|e| crate::Error::BadRequest(e.to_string()))?;
                out.push((job.to_id, notification));
            }
        }
        Ok(out)
    }

    /// Asserts `to` was sent exactly `count` notifications of type `N` (not yet delivered):
    /// `app.assert_notified::<RsvpReceived>(&organizer, 1).await?`.
    #[track_caller]
    pub fn assert_notified<'a, N: crate::notify::Notify>(
        &'a self,
        to: &'a N::To,
        count: usize,
    ) -> impl Future<Output = Result> + 'a {
        let caller = std::panic::Location::caller();
        async move {
            let sent: Vec<i64> = self.notified::<N>().await?.into_iter().map(|(id, _)| id).collect();
            let to_id = to.id().get();
            let mine = sent.iter().filter(|id| **id == to_id).count();
            assert_eq!(
                mine,
                count,
                "{caller}: expected {count} `{}` notification(s) to {} #{to_id}; queued for: {sent:?}",
                N::NAME,
                <N::To as Record>::TABLE
            );
            Ok(())
        }
    }

    /// Renders one mail's preview: `app.render_mail_preview::<WelcomeEmail>()`.
    pub async fn render_mail_preview<M: crate::mail::Preview>(&self) -> Result<crate::mail::Message> {
        M::preview(self.app.ctx()).await?.message()
    }

    /// Renders every mail preview (`#[mail(.., preview)]`), so a broken one fails a test.
    /// Returns `(name, message)` pairs.
    pub async fn render_mail_previews(&self) -> Result<Vec<(&'static str, crate::mail::Message)>> {
        let mut out = vec![];
        for def in crate::mail::previews() {
            let message = (def.render)(self.app.ctx().clone())
                .await
                .map_err(|e| crate::Error::Job(format!("the `{}` mail preview failed: {e}", def.name)))?;
            out.push((def.name, message));
        }
        Ok(out)
    }

    /// Mail delivered so far, oldest first. `deliver_later` queues a job: run
    /// `app.perform_enqueued_jobs()` first.
    pub async fn outbox(&self) -> Result<Vec<crate::mail::Message>> {
        Ok(self.db.outbox().await?.into_iter().map(|(_, m)| m).collect())
    }

    /// Asserts mail to `to` was delivered with `needle` in its subject or body. Returns it.
    #[track_caller]
    pub fn assert_mailed<'a>(
        &'a self,
        to: &'a str,
        needle: &'a str,
    ) -> impl Future<Output = Result<crate::mail::Message>> + 'a {
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

    /// Webhook delivery attempts so far (captured, or answered by `receive_webhooks`),
    /// oldest first. `deliver_later` queues a job: run `app.perform_enqueued_jobs()` first.
    pub async fn webhooks_sent(&self) -> Result<Vec<crate::webhooks::Delivery>> {
        let mut sent = self.db.recent_webhook_deliveries("", i64::MAX).await?;
        sent.reverse();
        Ok(sent)
    }

    /// Asserts a webhook for `event` was delivered (or captured). Returns the latest one.
    #[track_caller]
    pub fn assert_webhook_sent<'a>(
        &'a self,
        event: &'a str,
    ) -> impl Future<Output = Result<crate::webhooks::Delivery>> + 'a {
        let caller = std::panic::Location::caller();
        async move {
            let sent = self.webhooks_sent().await?;
            if let Some(found) = sent.iter().rev().find(|d| d.event == event) {
                return Ok(found.clone());
            }
            let queued = self.db.queued_payloads("DeliverWebhook").await?.len();
            let hint = if queued > 0 {
                format!(" ({queued} queued; run `app.perform_enqueued_jobs().await?` first)")
            } else {
                String::new()
            };
            let events: Vec<&str> = sent.iter().map(|d| d.event.as_str()).collect();
            panic!("{caller}: expected a `{event}` webhook{hint}; sent: {events:?}")
        }
    }

    /// Sends this app's webhook deliveries to `receive` instead of capturing them: it gets
    /// each signed request and answers with a status and body, so tests can exercise
    /// verification, retries (any non-2xx) and `410 Gone` without a network.
    pub fn receive_webhooks(
        &self,
        receive: impl Fn(&crate::webhooks::Received) -> (u16, String) + Send + Sync + 'static,
    ) {
        if let Some(receiver) = self.app.ctx().try_get::<crate::webhooks::TestReceiver>() {
            receiver.set(std::sync::Arc::new(receive));
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

    /// Signs in the `table` row `id` on the next request (`<app> request --as`).
    pub(crate) fn sign_in_row(&self, table: &'static str, id: i64) {
        *self.pending_sign_in.lock().unwrap_or_else(|e| e.into_inner()) = Some((table, id));
    }

    /// Stores the sign-in `sign_in_as` asked for (done on the first request otherwise).
    pub(crate) async fn store_pending_sign_in(&self) {
        let pending = self.pending_sign_in.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((table, id)) = pending {
            let token = crate::auth::create_sign_in(&self.db, table, id, "kiln test client", "127.0.0.1")
                .await
                .expect("storing the test sign-in");
            self.session_insert(&format!("current.{table}"), Value::from(token));
            self.session_insert(&format!("current.{table}.seen"), Value::from(jiff::Timestamp::now().as_second()));
        }
    }

    /// A string the session holds, e.g. the sign-in token under `current.users`.
    pub(crate) fn session_string(&self, key: &str) -> Option<String> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).get(key).and_then(|v| v.as_str().map(str::to_owned))
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
        self.send(method, path, accept, Some((content_type, Body::from(body)))).await
    }

    /// The urlencoded body for `fields`, with the session's CSRF token when `csrf`.
    pub(crate) fn form_body(&self, fields: &[(&str, &str)], csrf: bool) -> (&'static str, String) {
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
        self.send(method, path, None, Some(("application/x-www-form-urlencoded", Body::from(body)))).await
    }

    /// Submits a form with files, as a browser does for `form_start(.., multipart=true)`:
    /// `client.post_multipart(path, &[("title", "Hi")], &[("cover", Upload::png("cover.png"))])`.
    pub async fn post_multipart(
        &self,
        path: impl AsRef<str>,
        fields: &[(&str, &str)],
        files: &[(&str, Upload)],
    ) -> TestResponse {
        self.multipart(Method::POST, path.as_ref(), fields, files).await
    }

    pub async fn patch_multipart(
        &self,
        path: impl AsRef<str>,
        fields: &[(&str, &str)],
        files: &[(&str, Upload)],
    ) -> TestResponse {
        // Browsers send PATCH as a POST with `_method`.
        let mut fields = fields.to_vec();
        fields.push(("_method", "PATCH"));
        self.multipart(Method::POST, path.as_ref(), &fields, files).await
    }

    async fn multipart(
        &self,
        method: Method,
        path: &str,
        fields: &[(&str, &str)],
        files: &[(&str, Upload)],
    ) -> TestResponse {
        let (content_type, body) = self.multipart_body(fields, files);
        self.send(method, path, None, Some((&content_type, Body::from(body)))).await
    }

    /// The `multipart/form-data` body for `fields` and `files`, with the session's CSRF token.
    pub(crate) fn multipart_body(&self, fields: &[(&str, &str)], files: &[(&str, Upload)]) -> (String, Vec<u8>) {
        let token = {
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let token = session.entry("_csrf").or_insert(Value::from(TEST_CSRF));
            token.as_str().unwrap_or(TEST_CSRF).to_owned()
        };
        let boundary = "kiln-test-boundary-7MA4YWxkTrZu0gW";
        let mut body: Vec<u8> = vec![];
        for (name, value) in fields.iter().chain([&("_csrf", token.as_str())]) {
            body.extend(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        for (name, file) in files {
            body.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                    file.filename, file.content_type
                )
                .as_bytes(),
            );
            body.extend(&file.bytes);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    /// Sends a JSON body, as an API client does (no cookies or CSRF token needed).
    pub async fn post_json(&self, path: impl AsRef<str>, body: &serde_json::Value) -> TestResponse {
        self.send(Method::POST, path.as_ref(), None, Some(("application/json", Body::from(body.to_string())))).await
    }

    async fn send(&self, method: Method, path: &str, accept: Option<&str>, body: Option<(&str, Body)>) -> TestResponse {
        self.send_with(method, path, accept, body, &[]).await
    }

    /// Uploads `file` the way Kiln's JavaScript does (and Lexxy's attachments): the details to
    /// `/kiln/direct_uploads`, then the bytes to the `PUT` URL it answers with. Returns the
    /// blob (`upload_token` is what a form field submits to keep it).
    pub async fn direct_upload(&self, file: Upload) -> serde_json::Value {
        let token = self.csrf_token();
        let start = serde_json::json!({
            "blob": { "filename": file.filename, "content_type": file.content_type, "byte_size": file.bytes.len() }
        });
        let created = self
            .send_with(
                Method::POST,
                "/kiln/direct_uploads",
                Some("application/json"),
                Some(("application/json", Body::from(start.to_string()))),
                &[("x-csrf-token", &token)],
            )
            .await;
        assert_eq!(created.status.as_u16(), 200, "starting the direct upload failed: {}", created.text);
        let blob = created.json();
        let put = blob["direct_upload"]["url"].as_str().expect("a PUT URL").to_owned();
        let stored =
            self.send_with(Method::PUT, &put, None, Some((&file.content_type, Body::from(file.bytes))), &[]).await;
        assert_eq!(stored.status.as_u16(), 204, "storing the direct upload failed: {}", stored.text);
        blob
    }

    /// A JSON body sent as a page's JavaScript does: with the `X-CSRF-Token` header.
    pub async fn post_json_with_csrf(
        &self,
        path: impl AsRef<str>,
        body: &serde_json::Value,
        token: &str,
    ) -> TestResponse {
        self.send_with(
            Method::POST,
            path.as_ref(),
            Some("application/json"),
            Some(("application/json", Body::from(body.to_string()))),
            &[("x-csrf-token", token)],
        )
        .await
    }

    /// The CSRF token the session holds (what a page's `<meta name="csrf-token">` carries).
    pub fn csrf_token(&self) -> String {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let token = session.entry("_csrf").or_insert(Value::from(TEST_CSRF));
        token.as_str().unwrap_or(TEST_CSRF).to_owned()
    }

    pub(crate) async fn send_with(
        &self,
        method: Method,
        path: &str,
        accept: Option<&str>,
        body: Option<(&str, Body)>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.store_pending_sign_in().await;
        let mut builder = Request::builder().method(method).uri(path).header(header::USER_AGENT, "kiln test client");
        if let Some(accept) = accept {
            builder = builder.header(header::ACCEPT, accept);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !session.is_empty() {
            builder = builder.header(header::COOKIE, format!("{COOKIE}={}", self.key.sign(&session)));
        }
        let body = match body {
            Some((content_type, body)) => {
                builder = builder.header(header::CONTENT_TYPE, content_type);
                body
            }
            None => Body::empty(),
        };
        let request = builder.body(body).expect("valid test request");
        let response = self.router.clone().oneshot(request).await.expect("router is infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let error_report = response.extensions().get::<crate::error_page::ReportMarkdown>().map(|r| r.0.clone());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("readable body");

        let mut flash = BTreeMap::new();
        if let Some(updated) = self.read_session_cookie(&headers) {
            if let Some(Value::Object(f)) = updated.get("_flash") {
                flash = f.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect();
            }
            *self.session.lock().unwrap_or_else(|e| e.into_inner()) = updated;
        }
        TestResponse {
            status,
            headers,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            bytes: bytes.to_vec(),
            flash,
            error_report,
            key: self.key.clone(),
        }
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
    /// The body as sent (images, downloads).
    pub bytes: Vec<u8>,
    /// Flash set by this response (for the next page), e.g. `notice`.
    pub flash: BTreeMap<String, String>,
    /// In development, the error report as Markdown when the response is an error page,
    /// whatever format it was sent in.
    pub error_report: Option<String>,
    key: SecretKey,
}

impl TestResponse {
    /// The page listens to `stream` (`{{ turbo_stream_from(stream) }}`).
    #[track_caller]
    pub fn assert_streams_from(&self, stream: &str) -> &Self {
        let tag = crate::realtime::source_tag(&self.key, stream);
        assert!(
            self.text.contains(&tag),
            "expected the page to stream from {stream:?} (`{tag}`); body:\n{}",
            self.text
        );
        self
    }

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

/// Ids of jobs named `name` that haven't finished, oldest first.
pub async fn job_ids(db: &Db, name: &str) -> Result<Vec<i64>> {
    let rows = db
        .role(crate::db::Role::Queue)
        .fetch_all(
            "SELECT \"id\" FROM \"kiln_jobs\" WHERE \"name\" = ? ORDER BY \"id\"",
            vec![crate::Value::Text(name.to_owned())],
        )
        .await?;
    Ok(rows.iter().map(|r| sqlx::Row::get(r, 0)).collect())
}

/// Attempts used by unfinished jobs named `name`, oldest first.
pub async fn job_attempts(db: &Db, name: &str) -> Result<Vec<i64>> {
    let rows = db
        .role(crate::db::Role::Queue)
        .fetch_all(
            "SELECT \"attempts\" FROM \"kiln_jobs\" WHERE \"name\" = ? ORDER BY \"id\"",
            vec![crate::Value::Text(name.to_owned())],
        )
        .await?;
    Ok(rows.iter().map(|r| sqlx::Row::get(r, 0)).collect())
}

/// The database for one test.
///
/// - SQLite apps: a private in-memory database with the app's migrations applied.
/// - Postgres apps (`db/snapshot.json` says so): `TEST_DATABASE_URL`, else
///   `postgres://localhost/<app>_test`, with each test in its own fresh schema.
/// - `KILN_TEST_DATABASE_URL=postgres://..` runs any app's tests on Postgres. When the app's
///   migrations are for another backend, the schema comes from the record structs.
async fn test_database(root: &Path, migrations: &Path) -> Result<Db> {
    let snapshot = crate::Snapshot::load(&root.join("db/snapshot.json"))?;
    let url = std::env::var("KILN_TEST_DATABASE_URL").ok().or_else(|| match snapshot.dialect().name() {
        "postgres" => Some(
            std::env::var("TEST_DATABASE_URL")
                .unwrap_or_else(|_| format!("postgres://localhost/{}_test", crate::cli::app_name(root))),
        ),
        _ => None,
    });
    let Some(url) = url else {
        let db = Db::memory().await?;
        db.migrate(migrations).await?;
        return Ok(db);
    };
    if crate::dialect::for_url(&url)?.name() != "postgres" {
        let db = Db::connect(&url).await?;
        db.migrate(migrations).await?;
        return Ok(db);
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let schema = format!("kiln_t_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    drop_stale_schemas(&url).await;
    let db = Db::postgres_schema(&url, &schema).await?;
    if snapshot.dialect().name() == "postgres" {
        db.migrate(migrations).await?;
    } else {
        let schema = crate::migrate::diff_for(
            &crate::Snapshot::default(),
            &crate::Snapshot::from_records(),
            &crate::dialect::Postgres,
        );
        for statement in &schema.statements {
            db.execute_script(statement).await?;
        }
    }
    Ok(db)
}

/// Schemas left by earlier test runs (other processes), dropped once per process.
async fn drop_stale_schemas(url: &str) {
    static DONE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    DONE.get_or_init(|| async {
        let Ok(db) = Db::connect_or_create(url).await else { return };
        let mine = format!("kiln_t_{}_", std::process::id());
        let Ok(rows) = db
            .fetch_all("SELECT CAST(\"schema_name\" AS TEXT) FROM information_schema.schemata WHERE \"schema_name\" LIKE 'kiln_t_%'", vec![])
            .await
        else {
            return;
        };
        for row in rows {
            let name: String = sqlx::Row::get(&row, 0);
            if !name.starts_with(&mine) {
                let _ = db.execute_script(&format!("DROP SCHEMA IF EXISTS \"{name}\" CASCADE")).await;
            }
        }
    })
    .await;
}
