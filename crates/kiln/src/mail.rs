//! Email: a typed struct per message, rendered through templates and delivered by a job.
//!
//! ```ignore
//! #[derive(Mail, Serialize)]
//! #[mail(template = "mail/comment_notification", subject = "New comment on {{ post.title }}")]
//! pub struct CommentNotification {
//!     pub to: String,
//!     pub post: Post,
//!     pub comment: Comment,
//! }
//!
//! CommentNotification { to: author.email.clone(), post, comment }.deliver_later(&tx).await?;
//! ```
//!
//! The HTML body is `templates/mail/comment_notification.html.jinja` (inside
//! `layouts/mail.html.jinja` when it exists); a `.txt.jinja` beside it adds a plain-text
//! part. Messages render when they're queued, so the delivery job carries finished text.
//!
//! Where mail goes is `MAIL_URL`: `smtp://user:pass@host:587` (STARTTLS), `smtps://..`
//! (TLS), or `outbox` (the development and test default), which stores messages in
//! `kiln_mail` for `/kiln/mail` and `app.outbox()`. `MAIL_FROM` is the sender and
//! `APP_URL` the base for `url(..)` in templates.

use std::{future::Future, sync::OnceLock};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    Ctx, Db, Error, Result, Secret, Timestamp, Value,
    config::Config,
    jobs::{Enqueue, Perform},
    view::escape_attribute,
};

/// Implemented by `#[derive(Mail)]`.
pub trait Mail: Serialize + Send + Sync {
    const ROOT: &'static str;
    const TEMPLATE: &'static str;
    const TEXT_TEMPLATE: Option<&'static str>;
    const LAYOUT: Option<&'static str>;
    /// A template string, rendered with the struct's fields.
    const SUBJECT: &'static str;

    fn to(&self) -> &str;

    /// The finished message.
    fn message(&self) -> Result<Message> {
        let html = crate::view::render(Self::ROOT, Self::TEMPLATE, Self::LAYOUT, self)?;
        let text = Self::TEXT_TEMPLATE.map(|t| crate::view::render(Self::ROOT, t, None, self)).transpose()?;
        let subject = crate::view::render_str(Self::ROOT, Self::SUBJECT, self)?;
        Ok(Message {
            to: self.to().to_owned(),
            from: settings()?.mail_from.clone(),
            subject: subject.trim().to_owned(),
            html,
            text,
        })
    }

    /// Queues delivery. Inside a transaction, the mail goes out only if it commits.
    fn deliver_later(&self, db: &Db) -> impl Future<Output = Result> + Send {
        let message = self.message();
        async move {
            Deliver { message: message? }.enqueue(db).await?;
            Ok(())
        }
    }

    /// Sends now, in this task. Prefer `deliver_later`: SMTP is slow and can fail.
    fn deliver_now(&self, ctx: &Ctx) -> impl Future<Output = Result> + Send {
        let message = self.message();
        async move { send(ctx, message?).await }
    }
}

/// Sample data for a mail, shown at `/kiln/mail/previews` without sending. Opt in with
/// `#[mail(.., preview)]`; previews may read the database (`Post::query().one(&ctx.db)`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` asks for a preview (`#[mail(.., preview)]`) but has none",
    label = "missing `impl Preview for {Self}`",
    note = "add `impl Preview for {Self} {{ async fn preview(ctx: &Ctx) -> Result<Self> {{ .. }} }}`, or remove `preview`"
)]
pub trait Preview: Mail + Sized + 'static {
    fn preview(ctx: &Ctx) -> impl Future<Output = Result<Self>> + Send;
}

/// A previewable mail, registered by `#[mail(.., preview)]`.
pub struct PreviewDef {
    pub name: &'static str,
    pub render: fn(Ctx) -> crate::BoxFuture<'static, Result<Message>>,
}

inventory::collect!(PreviewDef);

#[doc(hidden)]
pub fn render_preview<M: Preview>(ctx: Ctx) -> crate::BoxFuture<'static, Result<Message>> {
    Box::pin(async move { M::preview(&ctx).await?.message() })
}

/// Every previewable mail, by name.
pub fn previews() -> Vec<&'static PreviewDef> {
    let mut all: Vec<_> = inventory::iter::<PreviewDef>.into_iter().collect();
    all.sort_by_key(|p| p.name);
    all
}

/// A rendered email.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Message {
    pub to: String,
    pub from: String,
    pub subject: String,
    pub html: String,
    pub text: Option<String>,
}

impl Message {
    /// Whether the subject or either body contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        self.subject.contains(needle)
            || self.html.contains(needle)
            || self.text.as_deref().is_some_and(|t| t.contains(needle))
    }
}

#[derive(crate::Config)]
struct MailSettings {
    #[config(dev_default = "outbox")]
    mail_url: Secret<String>,
    #[config(default = "app@localhost")]
    mail_from: String,
    #[config(default = "http://localhost:3000")]
    app_url: String,
}

fn settings() -> Result<&'static MailSettings> {
    static SETTINGS: OnceLock<MailSettings> = OnceLock::new();
    if let Some(settings) = SETTINGS.get() {
        return Ok(settings);
    }
    let loaded = MailSettings::load()?;
    Ok(SETTINGS.get_or_init(|| loaded))
}

/// `APP_URL` + `path`, for links in emails (`{{ url(path("posts::show", post.slug)) }}`).
pub(crate) fn absolute_url(path: &str) -> String {
    let base = settings().map(|s| s.app_url.as_str()).unwrap_or("http://localhost:3000");
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The job behind `deliver_later`.
#[derive(crate::Job, Serialize, Deserialize)]
#[job(queue = "mail", attempts = 8)]
pub struct Deliver {
    pub message: Message,
}

impl Perform for Deliver {
    async fn perform(self, ctx: &Ctx) -> Result {
        send(ctx, self.message).await
    }
}

async fn send(ctx: &Ctx, message: Message) -> Result {
    let url = settings()?.mail_url.expose();
    if url == "outbox" {
        tracing::info!(target: "kiln::mail", to = %message.to, subject = %message.subject, "mail stored in the outbox (/kiln/mail)");
        return ctx.db.store_mail(&message).await;
    }
    smtp(url, &message).await?;
    tracing::info!(target: "kiln::mail", to = %message.to, subject = %message.subject, "mail sent");
    Ok(())
}

async fn smtp(url: &str, message: &Message) -> Result {
    use lettre::{
        AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
        message::{MultiPart, SinglePart, header::ContentType},
    };
    let bad = |what: &str, e: &dyn std::fmt::Display| Error::Job(format!("mail: {what}: {e}"));
    let builder = lettre::Message::builder()
        .from(message.from.parse().map_err(|e| bad("MAIL_FROM isn't an address", &e))?)
        .to(message.to.parse().map_err(|e| bad("bad recipient", &e))?)
        .subject(&message.subject);
    let html = SinglePart::builder().header(ContentType::TEXT_HTML).body(message.html.clone());
    let email = match &message.text {
        Some(text) => builder.multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text.clone()))
                .singlepart(html),
        ),
        None => builder.singlepart(html),
    }
    .map_err(|e| bad("couldn't build the message", &e))?;
    // Built per message: mail volume is low, and a long-lived connection would go stale.
    let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
        .map_err(|e| bad("MAIL_URL isn't a valid smtp:// or smtps:// URL", &e))?
        .build();
    transport.send(email).await.map_err(|e| bad("SMTP delivery failed", &e))?;
    Ok(())
}

impl Db {
    pub(crate) async fn with_mail_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_mail\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"message\" TEXT NOT NULL, \
             \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.with_session_table().await
    }

    async fn store_mail(&self, message: &Message) -> Result {
        let json = serde_json::to_string(message).map_err(|e| Error::Job(e.to_string()))?;
        self.fetch_all(
            "INSERT INTO \"kiln_mail\" (\"message\", \"created_at\") VALUES (?, ?) RETURNING \"id\"",
            vec![Value::Text(json), Value::Integer(Timestamp::now().as_jiff().as_second())],
        )
        .await?;
        Ok(())
    }

    /// Mail stored by the `outbox` transport, oldest first: `(id, message)`.
    pub async fn outbox(&self) -> Result<Vec<(i64, Message)>> {
        let rows = self.fetch_all("SELECT \"id\", \"message\" FROM \"kiln_mail\" ORDER BY \"id\"", vec![]).await?;
        Ok(rows.iter().filter_map(|r| Some((r.get(0), serde_json::from_str(&r.get::<String, _>(1)).ok()?))).collect())
    }
}

/// `/kiln/mail` and `/kiln/mail/<id>`: the development outbox.
pub(crate) async fn serve_outbox(ctx: &Ctx, path: &str, query: &crate::devtools::Query) -> Option<Response> {
    let db = &ctx.db;
    let rest = path.strip_prefix("/kiln/mail")?;
    let tabs = "<div class=tabs><a href=\"/kiln/mail\"{outbox}>Outbox</a><a href=\"/kiln/mail/previews\"{previews}>Previews</a></div>";
    if let Some(name) = rest.strip_prefix("/previews") {
        let tabs = tabs.replace("{outbox}", "").replace("{previews}", " aria-current=page");
        return Some(preview_page(ctx, name.trim_start_matches('/'), &tabs, query).await);
    }
    let tabs = tabs.replace("{outbox}", " aria-current=page").replace("{previews}", "");
    let outbox = match db.outbox().await {
        Ok(outbox) => outbox,
        Err(e) => return Some(e.into_response()),
    };
    let body = match rest.trim_start_matches('/') {
        "" => {
            let shown: Vec<_> = outbox
                .iter()
                .rev()
                .filter(|(_, m)| {
                    crate::devtools::matches(query, &[&m.subject, &m.to, &m.from, m.text.as_deref().unwrap_or(&m.html)])
                })
                .collect();
            let rows: String = shown
                .iter()
                .map(|(id, m)| {
                    format!(
                        "<a class=row href=\"/kiln/mail/{id}\"><b>{}</b><span>{}</span></a>",
                        escape_attribute(&m.subject),
                        escape_attribute(&m.to)
                    )
                })
                .collect();
            let rows = if outbox.is_empty() {
                "<p class=empty>No mail yet.</p>".to_owned()
            } else {
                format!(
                    "{}{}{rows}",
                    crate::devtools::search_bar("/kiln/mail", query, "Subject, recipient or text", &[]),
                    crate::devtools::showing(shown.len(), outbox.len())
                )
            };
            format!(
                "{tabs}<p class=muted>Mail sent in development and tests is stored here instead of delivered.</p>{rows}"
            )
        }
        id => {
            let Some((_, m)) = id.parse::<i64>().ok().and_then(|id| outbox.iter().find(|(i, _)| *i == id)) else {
                return Some((StatusCode::NOT_FOUND, "no such mail").into_response());
            };
            let text = m
                .text
                .as_deref()
                .map(|t| format!("<h2>Text</h2><pre>{}</pre>", escape_attribute(t)))
                .unwrap_or_default();
            format!(
                "<p><a href=\"/kiln/mail\">← All mail</a></p><h2>{}</h2><dl><dt>To</dt><dd>{}</dd><dt>From</dt><dd>{}</dd></dl>\
                 <iframe sandbox srcdoc=\"{}\"></iframe>{text}",
                escape_attribute(&m.subject),
                escape_attribute(&m.to),
                escape_attribute(&m.from),
                escape_attribute(&m.html),
            )
        }
    };
    Some(crate::devtools::page("mail", "Mail", &body))
}

/// One message's headers, HTML (sandboxed) and text.
fn message_html(m: &Message) -> String {
    let text =
        m.text.as_deref().map(|t| format!("<h2>Text</h2><pre>{}</pre>", escape_attribute(t))).unwrap_or_default();
    format!(
        "<h2>{}</h2><dl><dt>To</dt><dd>{}</dd><dt>From</dt><dd>{}</dd></dl><iframe sandbox srcdoc=\"{}\"></iframe>{text}",
        escape_attribute(&m.subject),
        escape_attribute(&m.to),
        escape_attribute(&m.from),
        escape_attribute(&m.html),
    )
}

/// `/kiln/mail/previews` and `/kiln/mail/previews/<Name>`: mail rendered from sample data.
async fn preview_page(ctx: &Ctx, name: &str, tabs: &str, query: &crate::devtools::Query) -> Response {
    let all = previews();
    if name.is_empty() {
        let shown: Vec<_> = all.iter().filter(|p| crate::devtools::matches(query, &[p.name])).collect();
        let rows: String = shown
            .iter()
            .map(|p| {
                format!("<a class=row href=\"/kiln/mail/previews/{0}\"><b>{0}</b><span>preview</span></a>", p.name)
            })
            .collect();
        let body = if all.is_empty() {
            format!(
                "{tabs}<p class=empty>No previews. Add <code>preview</code> to a mail's <code>#[mail(..)]</code> and <code>impl Preview</code> for it.</p>"
            )
        } else {
            format!("{tabs}{}{rows}", crate::devtools::search_bar("/kiln/mail/previews", query, "Mail name", &[]))
        };
        return crate::devtools::page("mail", "Mail", &body);
    }
    let Some(def) = all.iter().find(|p| p.name == name) else {
        return (StatusCode::NOT_FOUND, "no such preview").into_response();
    };
    let body = match (def.render)(ctx.clone()).await {
        Ok(message) => message_html(&message),
        Err(e) => format!("<p class=notice>The preview failed: {}</p>", escape_attribute(&e.to_string())),
    };
    let body = format!("{tabs}<p><a href=\"/kiln/mail/previews\">← All previews</a> · not sent</p>{body}");
    crate::devtools::page("mail", &format!("Preview: {name}"), &body)
}
