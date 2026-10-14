//! Outbound webhooks: signed JSON POSTs to URLs your users register, retried by a job and
//! logged for inspection and redelivery.
//!
//! ```ignore
//! // The app owns its endpoints (url, secret, events, active) as a record.
//! let delivery_id = Webhook::new(&hook.url, hook.secret.expose())
//!     .for_record(&hook)                                   // log deliveries under it
//!     .deliver_later(&tx, "issue.opened", &IssueOpened { .. })
//!     .await?;
//!
//! ctx.db.webhook_deliveries(&hook, 50).await?;            // its delivery log, newest first
//! Webhook::new(&hook.url, hook.secret.expose()).for_record(&hook).redeliver(&ctx.db, id).await?;
//! ```
//!
//! Each request is `POST` with the payload's JSON as the body and these headers (the
//! `X-Kiln` prefix is configurable, see [`Options`]):
//!
//! - `X-Kiln-Event: issue.opened`
//! - `X-Kiln-Delivery: <uuid>`: the same on every retry and redelivery, so receivers can
//!   drop duplicates.
//! - `X-Kiln-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
//!
//! The signature format is Stripe's: the timestamp is signed with the body, so a captured
//! request can't be replayed later, and more `v1=` entries can be added while a secret is
//! rotated. GitHub's `X-Hub-Signature-256` (body only, no replay protection) can be sent as
//! well for receivers that already understand it: [`Options::hub_signature`].
//!
//! To verify, a receiver takes `t` and every `v1` from the header, computes
//! `hex(HMAC-SHA256(secret, t + "." + raw_body))`, compares it to each `v1` in constant
//! time, and rejects timestamps more than a few minutes from its clock. In Kiln that's
//! [`verify`].
//!
//! A 2xx response is success. Anything else, a timeout (10 s) or a connection error is a
//! failure, retried with backoff (15 attempts, about four hours). `410 Gone` means the
//! receiver wants no more: it isn't retried, and [`Options::on_gone`] runs so the app can
//! turn the endpoint off. Destinations on private networks are refused (see
//! [`crate::http`]) and not retried.
//!
//! Every attempt is a row in `kiln_webhook_deliveries` (request, response status, the first
//! 4 KB of the response body, timing, error), kept for `WEBHOOK_LOG_DAYS` (30) days and
//! shown at `/kiln/webhooks`. Secrets are never logged; they travel inside the delivery
//! job's payload, as they're needed to sign each retry.
//!
//! `WEBHOOK_DELIVERY` is `send` or `capture`; unset, it's `capture` in development and
//! tests: deliveries are signed and logged (status `captured`) but not sent. Tests read
//! them with `app.webhooks_sent()`, or answer them with `app.receive_webhooks(..)`.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::Row;

use crate::{
    BoxFuture, Ctx, Db, Error, Record, Result, Timestamp, Value,
    config::Config,
    jobs::{Enqueue, Perform},
    view::escape_attribute as esc,
};

/// How much of a response body the log keeps.
pub const RESPONSE_EXCERPT: usize = 4096;

/// One endpoint to deliver to: a URL and the secret its owner verifies signatures with.
#[derive(Clone)]
pub struct Webhook {
    url: String,
    secret: String,
    record: Option<(String, i64)>,
}

impl std::fmt::Debug for Webhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhook").field("url", &self.url).field("secret", &"[redacted]").finish()
    }
}

impl Webhook {
    pub fn new(url: impl Into<String>, secret: impl Into<String>) -> Self {
        Self { url: url.into(), secret: secret.into(), record: None }
    }

    /// Files deliveries under `record` (the app's webhook row), for
    /// `db.webhook_deliveries(&record, ..)`, redelivery and [`Options::on_gone`].
    pub fn for_record<R: Record>(mut self, record: &R) -> Self {
        self.record = Some((R::TABLE.to_owned(), record.id().get()));
        self
    }

    fn job<T: Serialize + ?Sized>(&self, event: &str, payload: &T) -> Result<DeliverWebhook> {
        let body = serde_json::to_string(payload)
            .map_err(|e| Error::BadRequest(format!("webhook `{event}` payload doesn't serialize: {e}")))?;
        Ok(DeliverWebhook {
            delivery_id: new_delivery_id(),
            url: self.url.clone(),
            secret: self.secret.clone(),
            event: event.to_owned(),
            body,
            record_table: self.record.as_ref().map(|(t, _)| t.clone()),
            record_id: self.record.as_ref().map(|(_, id)| *id),
            redelivery: false,
        })
    }

    /// Queues a delivery of `payload` (as JSON) for `event`. Inside a transaction, it goes
    /// out only if that commits. Returns the delivery id (the `X-Kiln-Delivery` header).
    pub async fn deliver_later<T: Serialize + ?Sized>(&self, db: &Db, event: &str, payload: &T) -> Result<String> {
        let job = self.job(event, payload)?;
        let id = job.delivery_id.clone();
        job.enqueue(db).await?;
        Ok(id)
    }

    /// Delivers now, once, without retries, and returns the logged attempt whatever its
    /// outcome. For "send a test ping" buttons; prefer `deliver_later` for events.
    pub async fn deliver_now<T: Serialize + ?Sized>(&self, ctx: &Ctx, event: &str, payload: &T) -> Result<Delivery> {
        attempt(ctx, &self.job(event, payload)?).await
    }

    /// Queues the logged delivery `id` again, to this webhook's current URL and signed with
    /// its current secret, keeping its event, body and delivery id. The delivery must have
    /// been filed under the same record (`for_record`), else it's `Error::NotFound`, so one
    /// owner can't replay another's.
    pub async fn redeliver(&self, db: &Db, id: i64) -> Result<String> {
        let original = db.webhook_delivery(id).await?.ok_or(Error::NotFound)?;
        let record = original.record_table.clone().zip(original.record_id);
        if record != self.record {
            return Err(Error::NotFound);
        }
        let job = DeliverWebhook {
            delivery_id: original.delivery_id.clone(),
            url: self.url.clone(),
            secret: self.secret.clone(),
            event: original.event,
            body: original.request_body,
            record_table: original.record_table,
            record_id: original.record_id,
            redelivery: true,
        };
        job.enqueue(db).await?;
        Ok(original.delivery_id)
    }
}

/// A random UUID (version 4).
fn new_delivery_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

type OnGone = Arc<dyn Fn(Ctx, Delivery) -> BoxFuture<'static, Result> + Send + Sync>;

/// App-wide webhook settings: `app.provide(webhooks::Options::new().header_prefix("X-Forked"))`.
#[derive(Clone)]
pub struct Options {
    prefix: String,
    hub_signature: bool,
    user_agent: String,
    on_gone: Option<OnGone>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            prefix: "X-Kiln".to_owned(),
            hub_signature: false,
            user_agent: format!("Kiln-Webhook/{}", env!("CARGO_PKG_VERSION")),
            on_gone: None,
        }
    }
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    /// The prefix of the event, delivery and signature headers: `X-Forked` sends
    /// `X-Forked-Event`, `X-Forked-Delivery` and `X-Forked-Signature`.
    pub fn header_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Also sends GitHub's `X-Hub-Signature-256: sha256=<hex HMAC of the body>`, for
    /// receivers built for GitHub. It signs no timestamp, so it can't stop replays.
    pub fn hub_signature(mut self) -> Self {
        self.hub_signature = true;
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Runs when a receiver answers `410 Gone`, typically to deactivate the endpoint
    /// (`delivery.record_id` is its id when it was sent `for_record`).
    pub fn on_gone<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(Ctx, Delivery) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result> + Send + 'static,
    {
        self.on_gone = Some(Arc::new(move |ctx, delivery| Box::pin(handler(ctx, delivery))));
        self
    }
}

#[derive(crate::Config)]
struct WebhookSettings {
    /// `send` or `capture`. Unset: `capture` outside production.
    webhook_delivery: Option<String>,
    #[config(default = "30")]
    webhook_log_days: i64,
}

fn settings() -> Result<&'static WebhookSettings> {
    static SETTINGS: OnceLock<WebhookSettings> = OnceLock::new();
    if let Some(settings) = SETTINGS.get() {
        return Ok(settings);
    }
    let loaded = WebhookSettings::load()?;
    Ok(SETTINGS.get_or_init(|| loaded))
}

fn capturing() -> Result<bool> {
    match settings()?.webhook_delivery.as_deref().map(str::trim) {
        Some("capture") => Ok(true),
        Some("send") => Ok(false),
        Some(other) => Err(Error::Config(format!("WEBHOOK_DELIVERY is `send` or `capture` (got `{other}`)"))),
        None => Ok(!crate::config::is_production()),
    }
}

fn client() -> &'static crate::http::Client {
    static CLIENT: OnceLock<crate::http::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::Client::builder().timeout(Duration::from_secs(10)).max_body(RESPONSE_EXCERPT).build()
    })
}

/// The job behind `deliver_later` and `redeliver`. Its payload holds the secret, which
/// signing each retry needs.
#[derive(crate::Job, Serialize, Deserialize)]
#[job(queue = "webhooks", attempts = 15, timeout = "60s")]
pub struct DeliverWebhook {
    pub delivery_id: String,
    pub url: String,
    secret: String,
    pub event: String,
    /// The JSON body, exactly as signed.
    pub body: String,
    pub record_table: Option<String>,
    pub record_id: Option<i64>,
    pub redelivery: bool,
}

impl Perform for DeliverWebhook {
    async fn perform(self, ctx: &Ctx) -> Result {
        let delivery = attempt(ctx, &self).await?;
        match delivery.status {
            Status::Failed => Err(Error::Job(format!(
                "webhook `{}` to {}: {}",
                self.event,
                host(&self.url),
                delivery.error.as_deref().unwrap_or("failed")
            ))),
            Status::Gone => {
                tracing::warn!(target: "kiln::webhooks", event = %self.event, host = %host(&self.url), "receiver answered 410 Gone; not retrying");
                match ctx.try_get::<Options>().and_then(|o| o.on_gone.clone()) {
                    Some(on_gone) => on_gone(ctx.clone(), delivery).await,
                    None => Ok(()),
                }
            }
            Status::Blocked => {
                tracing::warn!(target: "kiln::webhooks", event = %self.event, host = %host(&self.url), error = delivery.error.as_deref().unwrap_or(""), "webhook destination refused; not retrying");
                Ok(())
            }
            Status::Succeeded | Status::Captured => Ok(()),
        }
    }
}

/// The URL's host, for logs and errors: URLs can carry tokens in their path or query.
fn host(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| "?".into())
}

/// The signature header's value for `body` sent at `timestamp` (Unix seconds):
/// `t=<timestamp>,v1=<hex HMAC-SHA256 of "<timestamp>.<body>">`.
pub fn sign(secret: &str, timestamp: i64, body: &str) -> String {
    format!("t={timestamp},v1={}", hex(&mac(secret, &[timestamp.to_string().as_bytes(), b".", body.as_bytes()])))
}

/// GitHub's `X-Hub-Signature-256` value: `sha256=<hex HMAC-SHA256 of the body>`.
pub fn hub_signature(secret: &str, body: &str) -> String {
    format!("sha256={}", hex(&mac(secret, &[body.as_bytes()])))
}

fn mac(secret: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().to_vec()
}

/// Checks a `t=..,v1=..` signature header (Kiln's, or Stripe-style from anyone else) against
/// the raw request body: some `v1` must be the HMAC of `"<t>.<body>"` under `secret`, and
/// `t` within `tolerance` of now (five minutes is usual). Comparisons are constant-time.
///
/// ```ignore
/// let signature = headers.get("x-kiln-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
/// kiln::webhooks::verify(secret, signature, &body, SignedDuration::from_mins(5))?;
/// ```
pub fn verify(secret: &str, header: &str, body: &str, tolerance: SignedDuration) -> Result<()> {
    let reject = |why: &str| Err(Error::BadRequest(format!("webhook signature rejected: {why}")));
    let mut timestamp = None;
    let mut candidates = vec![];
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", sig)) => candidates.extend(unhex(sig)),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else { return reject("no timestamp (`t=`)") };
    if candidates.is_empty() {
        return reject("no `v1=` signature");
    }
    let now = Timestamp::now().as_jiff().as_second();
    if (now - timestamp).abs() > tolerance.as_secs() {
        return reject("the timestamp is outside the tolerance (a replay, or a clock off)");
    }
    let mut expected = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    expected.update(timestamp.to_string().as_bytes());
    expected.update(b".");
    expected.update(body.as_bytes());
    if candidates.iter().any(|c| expected.clone().verify_slice(c).is_ok()) {
        Ok(())
    } else {
        reject("no signature matches")
    }
}

/// Checks GitHub's `X-Hub-Signature-256: sha256=..` header against the raw body, in
/// constant time. It signs no timestamp, so it can't detect replays.
pub fn verify_hub_signature(secret: &str, header: &str, body: &str) -> Result<()> {
    let signature = header.trim().strip_prefix("sha256=").and_then(unhex);
    let mut expected = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    expected.update(body.as_bytes());
    match signature {
        Some(sig) if expected.verify_slice(&sig).is_ok() => Ok(()),
        _ => Err(Error::BadRequest("webhook signature rejected: X-Hub-Signature-256 doesn't match".into())),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// 2xx.
    Succeeded,
    /// Non-2xx (other than 410), a timeout or a connection error; retried.
    Failed,
    /// `410 Gone`: not retried.
    Gone,
    /// The URL is invalid or on a private network: not sent, not retried.
    Blocked,
    /// Signed and logged but not sent (`WEBHOOK_DELIVERY=capture`, the development and test
    /// default).
    Captured,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Succeeded => "succeeded",
            Status::Failed => "failed",
            Status::Gone => "gone",
            Status::Blocked => "blocked",
            Status::Captured => "captured",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "succeeded" => Status::Succeeded,
            "gone" => Status::Gone,
            "blocked" => Status::Blocked,
            "captured" => Status::Captured,
            _ => Status::Failed,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One logged attempt (a row of `kiln_webhook_deliveries`).
#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub id: i64,
    /// Shared by every attempt and redelivery of one event (the `X-Kiln-Delivery` header).
    pub delivery_id: String,
    pub event: String,
    pub url: String,
    pub record_table: Option<String>,
    pub record_id: Option<i64>,
    pub request_headers: Vec<(String, String)>,
    pub request_body: String,
    pub status: Status,
    pub response_status: Option<u16>,
    /// The first [`RESPONSE_EXCERPT`] bytes.
    pub response_body: Option<String>,
    pub error: Option<String>,
    pub duration_ms: i64,
    /// 1 for the first attempt of this delivery id, counting retries and redeliveries.
    pub attempt: i64,
    pub redelivery: bool,
    pub created_at: Timestamp,
}

impl Delivery {
    /// A request header, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.request_headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The request body, parsed.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.request_body).unwrap_or(serde_json::Value::Null)
    }

    pub fn succeeded(&self) -> bool {
        self.status == Status::Succeeded
    }
}

/// A delivery as a test receiver sees it (`app.receive_webhooks(..)`).
#[derive(Debug, Clone)]
pub struct Received {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

type Receive = Arc<dyn Fn(&Received) -> (u16, String) + Send + Sync>;

/// Where a test app's deliveries go once `app.receive_webhooks(..)` is called.
#[derive(Clone, Default)]
pub(crate) struct TestReceiver(Arc<Mutex<Option<Receive>>>);

impl TestReceiver {
    pub(crate) fn set(&self, receive: Receive) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(receive);
    }

    fn get(&self) -> Option<Receive> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Signs, sends (or captures) and logs one attempt.
async fn attempt(ctx: &Ctx, job: &DeliverWebhook) -> Result<Delivery> {
    let options = ctx.try_get::<Options>().cloned().unwrap_or_default();
    let timestamp = Timestamp::now().as_jiff().as_second();
    let prefix = &options.prefix;
    let mut headers = vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), options.user_agent.clone()),
        (format!("{prefix}-Event"), job.event.clone()),
        (format!("{prefix}-Delivery"), job.delivery_id.clone()),
        (format!("{prefix}-Signature"), sign(&job.secret, timestamp, &job.body)),
    ];
    if options.hub_signature {
        headers.push(("X-Hub-Signature-256".to_owned(), hub_signature(&job.secret, &job.body)));
    }
    let started = std::time::Instant::now();
    let receiver = ctx.try_get::<TestReceiver>().and_then(TestReceiver::get);
    let (status, response_status, response_body, error) = if let Some(receive) = receiver {
        let received = Received { url: job.url.clone(), headers: headers.clone(), body: job.body.clone() };
        let (code, body) = receive(&received);
        let (status, error) = judge(code);
        (status, Some(code), Some(excerpt(body.as_bytes())), error)
    } else if capturing()? {
        (Status::Captured, None, None, None)
    } else {
        let mut request = client().post(&job.url);
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        match request.body(job.body.clone()).send().await {
            Ok(response) => {
                let (status, error) = judge(response.status);
                (status, Some(response.status), Some(excerpt(&response.body)), error)
            }
            Err(e) if e.is_retryable() => (Status::Failed, None, None, Some(e.to_string())),
            Err(e) => (Status::Blocked, None, None, Some(e.to_string())),
        }
    };
    let duration_ms = started.elapsed().as_millis() as i64;
    let delivery =
        ctx.db.log_webhook_delivery(job, &headers, status, response_status, response_body, error, duration_ms).await?;
    tracing::info!(target: "kiln::webhooks", event = %job.event, delivery = %job.delivery_id, host = %host(&job.url), status = %status, attempt = delivery.attempt, "webhook delivery");
    Ok(delivery)
}

fn judge(code: u16) -> (Status, Option<String>) {
    match code {
        200..=299 => (Status::Succeeded, None),
        410 => (Status::Gone, Some("410 Gone".into())),
        code => (Status::Failed, Some(format!("HTTP {code}"))),
    }
}

fn excerpt(body: &[u8]) -> String {
    let cut = &body[..body.len().min(RESPONSE_EXCERPT)];
    String::from_utf8_lossy(cut).into_owned()
}

fn micros(ts: Timestamp) -> i64 {
    ts.as_jiff().as_microsecond()
}

fn at(us: i64) -> Timestamp {
    Timestamp::from_jiff(jiff::Timestamp::from_microsecond(us).unwrap_or_default())
}

const COLUMNS: &str = "\"id\", \"delivery_id\", \"event\", \"url\", \"record_table\", \"record_id\", \"request_headers\", \
     \"request_body\", \"status\", \"response_status\", \"response_body\", \"error\", \"duration_ms\", \"attempt\", \
     \"redelivery\", \"created_at\"";

fn from_row(r: &crate::Row) -> Delivery {
    Delivery {
        id: r.get(0),
        delivery_id: r.get(1),
        event: r.get(2),
        url: r.get(3),
        record_table: r.get(4),
        record_id: r.get(5),
        request_headers: serde_json::from_str(&r.get::<String, _>(6)).unwrap_or_default(),
        request_body: r.get(7),
        status: Status::parse(&r.get::<String, _>(8)),
        response_status: r.get::<Option<i64>, _>(9).map(|s| s as u16),
        response_body: r.get(10),
        error: r.get(11),
        duration_ms: r.get(12),
        attempt: r.get(13),
        redelivery: r.get::<i64, _>(14) != 0,
        created_at: at(r.get(15)),
    }
}

impl Db {
    pub(crate) async fn with_webhook_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_webhook_deliveries\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \
             \"delivery_id\" TEXT NOT NULL, \"event\" TEXT NOT NULL, \"url\" TEXT NOT NULL, \"record_table\" TEXT, \
             \"record_id\" INTEGER, \"request_headers\" TEXT NOT NULL, \"request_body\" TEXT NOT NULL, \
             \"status\" TEXT NOT NULL, \"response_status\" INTEGER, \"response_body\" TEXT, \"error\" TEXT, \
             \"duration_ms\" INTEGER NOT NULL, \"attempt\" INTEGER NOT NULL, \"redelivery\" INTEGER NOT NULL, \
             \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        for (name, columns) in [
            ("kiln_webhook_deliveries_record", "\"record_table\", \"record_id\", \"id\""),
            ("kiln_webhook_deliveries_delivery", "\"delivery_id\""),
            ("kiln_webhook_deliveries_created_at", "\"created_at\""),
        ] {
            self.execute(&format!("CREATE INDEX IF NOT EXISTS \"{name}\" ON \"kiln_webhook_deliveries\" ({columns})"))
                .await?;
        }
        Ok(self)
    }

    #[allow(clippy::too_many_arguments)]
    async fn log_webhook_delivery(
        &self,
        job: &DeliverWebhook,
        headers: &[(String, String)],
        status: Status,
        response_status: Option<u16>,
        response_body: Option<String>,
        error: Option<String>,
        duration_ms: i64,
    ) -> Result<Delivery> {
        let previous = self
            .fetch_all(
                "SELECT COUNT(*) FROM \"kiln_webhook_deliveries\" WHERE \"delivery_id\" = ?",
                vec![Value::Text(job.delivery_id.clone())],
            )
            .await?
            .first()
            .map_or(0, |r| r.get::<i64, _>(0));
        let text = |s: Option<String>| s.map_or(Value::Null, Value::Text);
        let rows = self
            .fetch_all(
                &format!(
                    "INSERT INTO \"kiln_webhook_deliveries\" (\"delivery_id\", \"event\", \"url\", \"record_table\", \"record_id\", \
                     \"request_headers\", \"request_body\", \"status\", \"response_status\", \"response_body\", \"error\", \
                     \"duration_ms\", \"attempt\", \"redelivery\", \"created_at\") \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {COLUMNS}"
                ),
                vec![
                    Value::Text(job.delivery_id.clone()),
                    Value::Text(job.event.clone()),
                    Value::Text(job.url.clone()),
                    text(job.record_table.clone()),
                    job.record_id.map_or(Value::Null, Value::Integer),
                    Value::Text(serde_json::to_string(headers).unwrap_or_default()),
                    Value::Text(job.body.clone()),
                    Value::Text(status.as_str().to_owned()),
                    response_status.map_or(Value::Null, |s| Value::Integer(s.into())),
                    text(response_body),
                    text(error),
                    Value::Integer(duration_ms),
                    Value::Integer(previous + 1),
                    Value::Integer(job.redelivery as i64),
                    Value::Integer(micros(Timestamp::now())),
                ],
            )
            .await?;
        rows.first().map(from_row).ok_or_else(|| Error::Integrity("logging a webhook delivery returned no row".into()))
    }

    /// `record`'s logged delivery attempts (sent with `for_record`), newest first.
    pub async fn webhook_deliveries<R: Record>(&self, record: &R, limit: i64) -> Result<Vec<Delivery>> {
        let rows = self
            .fetch_all(
                &format!(
                    "SELECT {COLUMNS} FROM \"kiln_webhook_deliveries\" WHERE \"record_table\" = ? AND \"record_id\" = ? \
                     ORDER BY \"id\" DESC LIMIT ?"
                ),
                vec![Value::Text(R::TABLE.to_owned()), Value::Integer(record.id().get()), Value::Integer(limit)],
            )
            .await?;
        Ok(rows.iter().map(from_row).collect())
    }

    /// Every logged attempt, newest first, optionally only those whose event, URL or
    /// delivery id contains `search`.
    pub async fn recent_webhook_deliveries(&self, search: &str, limit: i64) -> Result<Vec<Delivery>> {
        let like = format!("%{search}%");
        let rows = self
            .fetch_all(
                &format!(
                    "SELECT {COLUMNS} FROM \"kiln_webhook_deliveries\" WHERE LOWER(\"event\") LIKE LOWER(?) \
                     OR LOWER(\"url\") LIKE LOWER(?) OR \"delivery_id\" LIKE ? ORDER BY \"id\" DESC LIMIT ?"
                ),
                vec![Value::Text(like.clone()), Value::Text(like.clone()), Value::Text(like), Value::Integer(limit)],
            )
            .await?;
        Ok(rows.iter().map(from_row).collect())
    }

    /// One logged attempt. Check it belongs to the viewer before showing it (its
    /// `record_table` / `record_id`).
    pub async fn webhook_delivery(&self, id: i64) -> Result<Option<Delivery>> {
        let rows = self
            .fetch_all(
                &format!("SELECT {COLUMNS} FROM \"kiln_webhook_deliveries\" WHERE \"id\" = ?"),
                vec![Value::Integer(id)],
            )
            .await?;
        Ok(rows.first().map(from_row))
    }

    /// Every attempt sharing one delivery id, oldest first.
    pub async fn webhook_delivery_attempts(&self, delivery_id: &str) -> Result<Vec<Delivery>> {
        let rows = self
            .fetch_all(
                &format!("SELECT {COLUMNS} FROM \"kiln_webhook_deliveries\" WHERE \"delivery_id\" = ? ORDER BY \"id\""),
                vec![Value::Text(delivery_id.to_owned())],
            )
            .await?;
        Ok(rows.iter().map(from_row).collect())
    }
}

/// Deletes logged attempts older than `WEBHOOK_LOG_DAYS` (workers do this every few
/// minutes). How many went.
pub async fn purge_old_deliveries(db: &Db) -> Result<u64> {
    let days = settings()?.webhook_log_days.max(1);
    let cutoff = micros(Timestamp::now() - SignedDuration::from_hours(24 * days));
    let rows = db
        .fetch_all(
            "DELETE FROM \"kiln_webhook_deliveries\" WHERE \"created_at\" < ? RETURNING \"id\"",
            vec![Value::Integer(cutoff)],
        )
        .await?;
    Ok(rows.len() as u64)
}

/// `/kiln/webhooks` and `/kiln/webhooks/<id>`: the delivery log.
pub(crate) async fn serve_log(ctx: &Ctx, path: &str, query: &crate::devtools::Query) -> Option<Response> {
    let rest = path.strip_prefix("/kiln/webhooks")?.trim_matches('/');
    let body = if rest.is_empty() {
        let search = query.get("q").map(|q| q.trim()).unwrap_or("");
        let shown = match ctx.db.recent_webhook_deliveries(search, 200).await {
            Ok(shown) => shown,
            Err(e) => return Some(e.into_response()),
        };
        let rows: String = shown
            .iter()
            .map(|d| {
                let code = d.response_status.map(|s| s.to_string()).unwrap_or_else(|| "–".into());
                format!(
                    "<tr><td><a href=\"/kiln/webhooks/{}\">#{}</a></td><td><b>{}</b><pre class=muted>{}</pre></td><td><span class=\"pill {}\">{}</span></td><td>{code}</td><td>{}</td><td>{} ms</td></tr>",
                    d.id,
                    d.id,
                    esc(&d.event),
                    esc(&d.url),
                    pill(d.status),
                    d.status,
                    d.attempt,
                    d.duration_ms
                )
            })
            .collect();
        let table = if shown.is_empty() {
            "<p class=empty>No webhook deliveries yet.</p>".to_owned()
        } else {
            format!(
                "<div class=tbl><table><thead><tr><th>Id</th><th>Event</th><th>Status</th><th>HTTP</th><th>Attempt</th><th>Took</th></tr></thead><tbody>{rows}</tbody></table></div>"
            )
        };
        let mode = match capturing() {
            Ok(true) => "Deliveries are captured, not sent (<code>WEBHOOK_DELIVERY=send</code> sends them).",
            _ => "Deliveries are sent.",
        };
        format!(
            "<p class=muted>{mode} Every attempt is logged here for {} days.</p>{}{table}",
            settings().map_or(30, |s| s.webhook_log_days),
            crate::devtools::search_bar("/kiln/webhooks", query, "Event, URL or delivery id", &[])
        )
    } else {
        let found = match rest.parse::<i64>() {
            Ok(id) => ctx.db.webhook_delivery(id).await,
            Err(_) => Ok(None),
        };
        let d = match found {
            Ok(Some(d)) => d,
            Ok(None) => return Some((StatusCode::NOT_FOUND, "no such delivery").into_response()),
            Err(e) => return Some(e.into_response()),
        };
        let headers: String =
            d.request_headers.iter().map(|(k, v)| format!("<dt>{}</dt><dd>{}</dd>", esc(k), esc(v))).collect();
        let body = serde_json::from_str::<serde_json::Value>(&d.request_body)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| d.request_body.clone());
        format!(
            "<p><a href=\"/kiln/webhooks\">← All deliveries</a></p><h2>{} <span class=\"pill {}\">{}</span></h2>\
             <dl><dt>URL</dt><dd>{}</dd><dt>Delivery</dt><dd>{}</dd><dt>Attempt</dt><dd>{}{}</dd><dt>HTTP</dt><dd>{}</dd><dt>Took</dt><dd>{} ms</dd><dt>Error</dt><dd>{}</dd></dl>\
             <h2>Request headers</h2><dl>{headers}</dl><h2>Request body</h2><pre>{}</pre><h2>Response body</h2><pre>{}</pre>",
            esc(&d.event),
            pill(d.status),
            d.status,
            esc(&d.url),
            esc(&d.delivery_id),
            d.attempt,
            if d.redelivery { " (redelivery)" } else { "" },
            d.response_status.map(|s| s.to_string()).unwrap_or_else(|| "–".into()),
            d.duration_ms,
            esc(d.error.as_deref().unwrap_or("–")),
            esc(&body),
            esc(d.response_body.as_deref().unwrap_or("")),
        )
    };
    Some(crate::devtools::page("webhooks", "Webhooks", &body))
}

/// The dev tools' pill class (its color) for a status.
fn pill(status: Status) -> &'static str {
    match status {
        Status::Succeeded => "queued",
        Status::Captured => "",
        Status::Failed | Status::Gone | Status::Blocked => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_verify_and_reject_tampering() {
        let now = Timestamp::now().as_jiff().as_second();
        let body = r#"{"action":"opened"}"#;
        let header = sign("whsec", now, body);
        assert!(header.starts_with(&format!("t={now},v1=")));
        let tolerance = SignedDuration::from_mins(5);
        assert!(verify("whsec", &header, body, tolerance).is_ok());
        assert!(verify("other", &header, body, tolerance).is_err(), "wrong secret");
        assert!(verify("whsec", &header, r#"{"action":"closed"}"#, tolerance).is_err(), "changed body");
        let old = sign("whsec", now - 600, body);
        assert!(verify("whsec", &old, body, tolerance).is_err(), "replayed later");
        assert!(verify("whsec", "v1=abcd", body, tolerance).is_err(), "no timestamp");
        assert!(verify("whsec", &format!("t={now}"), body, tolerance).is_err(), "no signature");
        assert!(verify("whsec", &format!("t={now},v1=zz"), body, tolerance).is_err(), "not hex");
        // During a rotation the header carries one signature per secret.
        let rotated = format!("{header},v1={}", sign("new", now, body).split_once(",v1=").unwrap().1);
        assert!(verify("new", &rotated, body, tolerance).is_ok());
        assert!(verify("whsec", &rotated, body, tolerance).is_ok());
    }

    #[test]
    fn signatures_match_a_known_vector() {
        // echo -n '1700000000.{}' | openssl dgst -sha256 -hmac secret
        assert_eq!(
            sign("secret", 1_700_000_000, "{}"),
            "t=1700000000,v1=b8569b78799ff9e3cbff0fc2d63a33a2b57f3282abd07c37ae5e8e7d79a5f163"
        );
        assert_eq!(
            hub_signature("It's a Secret to Everybody", "Hello, World!"),
            // GitHub's documented example.
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        assert!(
            verify_hub_signature(
                "It's a Secret to Everybody",
                "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
                "Hello, World!"
            )
            .is_ok()
        );
        assert!(verify_hub_signature("x", "sha256=00", "Hello, World!").is_err());
    }

    #[test]
    fn delivery_ids_are_uuids() {
        let id = new_delivery_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_delivery_id());
    }

    #[test]
    fn logs_name_hosts_not_urls() {
        assert_eq!(host("https://hooks.example.com/services/T0/B0/secret-token?x=1"), "hooks.example.com");
        let hook = Webhook::new("https://example.com", "s3cret");
        assert!(!format!("{hook:?}").contains("s3cret"));
    }
}
