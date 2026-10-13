//! Notifications: one event, delivered to a person over the channels you choose.
//!
//! ```ignore
//! #[derive(Notification, Serialize, Deserialize)]
//! pub struct CommentPosted {
//!     pub comment_id: Id<Comment>,
//! }
//!
//! impl Notify for CommentPosted {
//!     type To = User;
//!
//!     async fn deliver(&self, ctx: &Ctx, to: &User, via: &mut Via) -> Result {
//!         let comment = Comment::find(self.comment_id).preload(comment::author).one_or_404(&ctx.db).await?;
//!         via.inbox(format!("{} commented", comment.author().name)).link(posts::show::path(..));
//!         if to.email_notifications {
//!             via.mail(CommentNotification { to: to.email.clone(), .. })?;
//!         }
//!         Ok(())
//!     }
//! }
//!
//! CommentPosted { comment_id: comment.id }.notify(&tx, &author).await?;
//! ```
//!
//! `notify` enqueues a delivery job (inside a transaction, only if it commits). The job
//! loads the recipient, runs `deliver`, and then writes everything it asked for in one
//! transaction: inbox entries, mail (each delivered by its own job) and broadcasts. A
//! recipient deleted in the meantime gets nothing.
//!
//! The inbox is `kiln_notifications`: `db.inbox(&user, 20)`, `db.unread_notifications(&user)`,
//! `db.mark_notification_read(&user, id)` and `db.mark_all_notifications_read(&user)`.

use std::future::Future;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::Row;

use crate::{
    BoxFuture, Ctx, Db, Error, Id, Record, Result, Timestamp, TurboStream, Value,
    jobs::{Enqueue, Perform},
    mail::{Deliver, Mail, Message},
};

/// Implemented by `#[derive(Notification)]`.
pub trait NotificationInfo: Serialize + DeserializeOwned + Send + Sync + 'static {
    const NAME: &'static str;
    /// What it's about, for the settings page: "Someone comments on your post".
    const DESCRIPTION: &'static str;
    /// Channels each person may turn off (`#[notification(optional(mail, inbox))]`). Mail
    /// by default; `optional()` for notices nobody may mute, like security alerts.
    const OPTIONAL: &'static [Channel] = &[Channel::Mail];
}

/// Where a notification can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Inbox,
    Mail,
    Broadcast,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Inbox => "inbox",
            Channel::Mail => "mail",
            Channel::Broadcast => "broadcast",
        }
    }
}

impl std::str::FromStr for Channel {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "inbox" => Ok(Channel::Inbox),
            "mail" => Ok(Channel::Mail),
            "broadcast" => Ok(Channel::Broadcast),
            other => Err(Error::BadRequest(format!("`{other}` isn't a channel (inbox, mail or broadcast)"))),
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a notification sends, and where. Write it for every `#[derive(Notification)]`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a notification but doesn't say how it's delivered",
    label = "missing `impl Notify for {Self}`",
    note = "add `impl Notify for {Self} {{ type To = User; async fn deliver(&self, ctx: &Ctx, to: &User, via: &mut Via) -> Result {{ .. }} }}`"
)]
pub trait Notify: NotificationInfo {
    /// Who receives it: a record people sign in as, usually `User`.
    type To: Record;

    /// Decides what `to` gets: `via.inbox(..)`, `via.mail(..)`, `via.broadcast(..)`. Runs
    /// in a job, so it can load fresh data.
    fn deliver(&self, ctx: &Ctx, to: &Self::To, via: &mut Via) -> impl Future<Output = Result> + Send;

    /// Sends it to `to`. Inside a transaction, only if it commits.
    fn notify(&self, db: &Db, to: &Self::To) -> impl Future<Output = Result<i64>> + Send {
        let job = serde_json::to_value(self).map(|payload| DeliverNotification {
            name: Self::NAME.to_owned(),
            to_table: <Self::To as Record>::TABLE.to_owned(),
            to_id: to.id().get(),
            payload,
        });
        async move {
            let job =
                job.map_err(|e| Error::BadRequest(format!("notification `{}` doesn't serialize: {e}", Self::NAME)))?;
            job.enqueue(db).await
        }
    }

    /// Sends it to each of `recipients`.
    fn notify_all<'a>(
        &'a self,
        db: &'a Db,
        recipients: &'a [Self::To],
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async move {
            for to in recipients {
                self.notify(db, to).await?;
            }
            Ok(())
        }
    }
}

/// The channels a delivery asked for. Nothing is written until `deliver` returns `Ok`.
/// Channels the recipient turned off are dropped here, so `deliver` doesn't check them.
#[derive(Default)]
pub struct Via {
    inbox: Vec<InboxEntry>,
    mail: Vec<Message>,
    broadcasts: Vec<(String, TurboStream)>,
    /// Channels the recipient turned off for this notification.
    off: Vec<Channel>,
}

/// An inbox entry being built: `via.inbox("Grace commented").link(path).body("..")`.
pub struct InboxEntry {
    /// Dropped because the recipient turned the inbox off.
    skip: bool,
    title: String,
    body: Option<String>,
    url: Option<String>,
}

impl InboxEntry {
    /// Where the entry leads, usually a typed path: `posts::show::path(&post.slug)`.
    pub fn link(&mut self, url: impl Into<String>) -> &mut Self {
        self.url = Some(url.into());
        self
    }

    pub fn body(&mut self, body: impl Into<String>) -> &mut Self {
        self.body = Some(body.into());
        self
    }
}

impl Via {
    /// Whether the recipient gets `channel` for this notification. Only needed to skip
    /// work; the channel methods already drop what they turned off.
    pub fn wants(&self, channel: Channel) -> bool {
        !self.off.contains(&channel)
    }

    /// An entry in the recipient's in-app inbox.
    pub fn inbox(&mut self, title: impl Into<String>) -> &mut InboxEntry {
        let skip = !self.wants(Channel::Inbox);
        self.inbox.push(InboxEntry { skip, title: title.into(), body: None, url: None });
        self.inbox.last_mut().expect("just pushed")
    }

    /// An email, rendered now and delivered by its own job (so a slow SMTP server doesn't
    /// hold up the other channels).
    pub fn mail(&mut self, mail: impl Mail) -> Result<()> {
        if self.wants(Channel::Mail) {
            self.mail.push(mail.message()?);
        }
        Ok(())
    }

    /// A Turbo Stream for pages subscribed to `stream`, e.g. to bump an unread badge.
    pub fn broadcast(&mut self, stream: impl Into<String>, turbo: TurboStream) {
        if self.wants(Channel::Broadcast) {
            self.broadcasts.push((stream.into(), turbo));
        }
    }
}

/// A notification type linked into the binary, registered by `#[derive(Notification)]`.
pub struct NotificationDef {
    pub name: &'static str,
    pub description: &'static str,
    /// The recipient record's table.
    pub to: fn() -> &'static str,
    pub optional: &'static [Channel],
    pub deliver: fn(Ctx, i64, serde_json::Value) -> BoxFuture<'static, Result>,
}

#[doc(hidden)]
pub fn recipient_table<N: Notify>() -> &'static str {
    <N::To as Record>::TABLE
}

inventory::collect!(NotificationDef);

/// Used by `#[derive(Notification)]`.
#[doc(hidden)]
pub fn deliver_notification<N: Notify>(ctx: Ctx, to_id: i64, payload: serde_json::Value) -> BoxFuture<'static, Result> {
    Box::pin(async move {
        let notification: N = serde_json::from_value(payload)
            .map_err(|e| Error::BadRequest(format!("notification `{}` has an unreadable payload: {e}", N::NAME)))?;
        let Some(to) = N::To::find(Id::new(to_id)).one(&ctx.db).await? else {
            tracing::info!(target: "kiln::notify", name = N::NAME, to_id, "recipient is gone; skipped");
            return Ok(());
        };
        let table = <N::To as Record>::TABLE;
        let off =
            ctx.db.channels_off(table, to_id, N::NAME).await?.into_iter().filter(|c| N::OPTIONAL.contains(c)).collect();
        let mut via = Via { off, ..Via::default() };
        notification.deliver(&ctx, &to, &mut via).await?;
        ctx.db
            .transaction(|tx| async move {
                for entry in via.inbox.into_iter().filter(|e| !e.skip) {
                    tx.fetch_all(
                        "INSERT INTO \"kiln_notifications\" (\"recipient_table\", \"recipient_id\", \"kind\", \"title\", \"body\", \"url\", \"created_at\") \
                         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING \"id\"",
                        vec![
                            Value::Text(table.to_owned()),
                            Value::Integer(to_id),
                            Value::Text(N::NAME.to_owned()),
                            Value::Text(entry.title),
                            entry.body.map_or(Value::Null, Value::Text),
                            entry.url.map_or(Value::Null, Value::Text),
                            Value::Integer(now()),
                        ],
                    )
                    .await?;
                }
                for message in via.mail {
                    Deliver { message }.enqueue(&tx).await?;
                }
                for (stream, turbo) in via.broadcasts {
                    tx.broadcast(&stream, turbo).await?;
                }
                Ok(())
            })
            .await
    })
}

/// The job behind `notify`.
#[derive(crate::Job, Serialize, Deserialize)]
#[job(queue = "notifications", attempts = 8)]
pub struct DeliverNotification {
    pub name: String,
    pub to_table: String,
    pub to_id: i64,
    pub payload: serde_json::Value,
}

impl Perform for DeliverNotification {
    async fn perform(self, ctx: &Ctx) -> Result {
        let def = inventory::iter::<NotificationDef>
            .into_iter()
            .find(|d| d.name == self.name)
            .ok_or_else(|| Error::Job(format!("no notification named `{}` is linked into this binary", self.name)))?;
        (def.deliver)(ctx.clone(), self.to_id, self.payload).await
    }
}

fn now() -> i64 {
    Timestamp::now().as_jiff().as_microsecond()
}

/// An entry in someone's inbox.
#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: i64,
    /// The notification's type, e.g. `CommentPosted`.
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<String>,
    pub read: bool,
    pub created_at: Timestamp,
}

/// One switch on a person's notification settings page.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationPref {
    /// The notification's type, or `*` for the "every notification" switch.
    pub kind: String,
    pub description: String,
    pub channel: Channel,
    pub enabled: bool,
}

impl Db {
    /// Channels `recipient` turned off for `kind`, including their "every notification"
    /// switches.
    async fn channels_off(&self, table: &str, recipient: i64, kind: &str) -> Result<Vec<Channel>> {
        let rows = self
            .fetch_all(
                "SELECT \"channel\" FROM \"kiln_notification_prefs\" WHERE \"recipient_table\" = ? AND \"recipient_id\" = ? \
                 AND \"kind\" IN (?, '*') AND \"enabled\" = 0",
                vec![Value::Text(table.to_owned()), Value::Integer(recipient), Value::Text(kind.to_owned())],
            )
            .await?;
        Ok(rows.iter().filter_map(|r| r.get::<String, _>(0).parse().ok()).collect())
    }

    /// Every switch `to` can flip: each notification type they can receive, times each
    /// channel it lets them turn off, plus an "every notification" switch per channel.
    pub async fn notification_prefs<T: Record>(&self, to: &T) -> Result<Vec<NotificationPref>> {
        let stored = self
            .fetch_all(
                "SELECT \"kind\", \"channel\", \"enabled\" FROM \"kiln_notification_prefs\" WHERE \"recipient_table\" = ? AND \"recipient_id\" = ?",
                vec![Value::Text(T::TABLE.to_owned()), Value::Integer(to.id().get())],
            )
            .await?;
        let enabled = |kind: &str, channel: Channel| {
            !stored.iter().any(|r| {
                r.get::<&str, _>(0) == kind && r.get::<&str, _>(1) == channel.as_str() && r.get::<i64, _>(2) == 0
            })
        };
        let mut defs: Vec<_> =
            inventory::iter::<NotificationDef>.into_iter().filter(|d| (d.to)() == T::TABLE).collect();
        defs.sort_by_key(|d| d.description);
        let mut prefs = vec![];
        let mut channels: Vec<Channel> = vec![];
        for def in &defs {
            for channel in def.optional {
                if !channels.contains(channel) {
                    channels.push(*channel);
                }
                prefs.push(NotificationPref {
                    kind: def.name.to_owned(),
                    description: def.description.to_owned(),
                    channel: *channel,
                    enabled: enabled(def.name, *channel),
                });
            }
        }
        for channel in channels {
            let label = match channel {
                Channel::Mail => "Every email",
                Channel::Inbox => "Every inbox notification",
                Channel::Broadcast => "Every live update",
            };
            prefs.insert(
                0,
                NotificationPref {
                    kind: "*".into(),
                    description: label.into(),
                    channel,
                    enabled: enabled("*", channel),
                },
            );
        }
        Ok(prefs)
    }

    /// Turns `channel` on or off for `kind` (or `*`, every notification) for `to`. Refused
    /// for channels the notification doesn't let people turn off.
    pub async fn set_notification_pref<T: Record>(
        &self,
        to: &T,
        kind: &str,
        channel: Channel,
        enabled: bool,
    ) -> Result<()> {
        if kind != "*" {
            let def = inventory::iter::<NotificationDef>
                .into_iter()
                .find(|d| d.name == kind && (d.to)() == T::TABLE)
                .ok_or_else(|| Error::BadRequest(format!("no notification `{kind}` for {}", T::TABLE)))?;
            if !def.optional.contains(&channel) {
                return Err(Error::BadRequest(format!("`{kind}` can't be turned off for {channel}")));
            }
        }
        self.fetch_all(
            "INSERT INTO \"kiln_notification_prefs\" (\"recipient_table\", \"recipient_id\", \"kind\", \"channel\", \"enabled\") \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (\"recipient_table\", \"recipient_id\", \"kind\", \"channel\") DO UPDATE SET \"enabled\" = excluded.\"enabled\" \
             RETURNING \"kind\"",
            vec![
                Value::Text(T::TABLE.to_owned()),
                Value::Integer(to.id().get()),
                Value::Text(kind.to_owned()),
                Value::Text(channel.as_str().to_owned()),
                Value::Integer(enabled as i64),
            ],
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn with_notification_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_notification_prefs\" (\"recipient_table\" TEXT NOT NULL, \"recipient_id\" INTEGER NOT NULL, \
             \"kind\" TEXT NOT NULL, \"channel\" TEXT NOT NULL, \"enabled\" INTEGER NOT NULL, \
             PRIMARY KEY (\"recipient_table\", \"recipient_id\", \"kind\", \"channel\"))",
        )
        .await?;
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_notifications\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \
             \"recipient_table\" TEXT NOT NULL, \"recipient_id\" INTEGER NOT NULL, \"kind\" TEXT NOT NULL, \
             \"title\" TEXT NOT NULL, \"body\" TEXT, \"url\" TEXT, \"read_at\" INTEGER, \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS \"kiln_notifications_recipient\" ON \"kiln_notifications\" (\"recipient_table\", \"recipient_id\", \"id\")",
        )
        .await?;
        Ok(self)
    }

    /// `to`'s newest inbox entries.
    pub async fn inbox<T: Record>(&self, to: &T, limit: i64) -> Result<Vec<InboxItem>> {
        let rows = self
            .fetch_all(
                "SELECT \"id\", \"kind\", \"title\", \"body\", \"url\", \"read_at\", \"created_at\" FROM \"kiln_notifications\" \
                 WHERE \"recipient_table\" = ? AND \"recipient_id\" = ? ORDER BY \"id\" DESC LIMIT ?",
                vec![Value::Text(T::TABLE.to_owned()), Value::Integer(to.id().get()), Value::Integer(limit)],
            )
            .await?;
        Ok(rows.iter().map(row_to_notification).collect())
    }

    pub async fn unread_notifications<T: Record>(&self, to: &T) -> Result<i64> {
        let rows = self
            .fetch_all(
                "SELECT COUNT(*) FROM \"kiln_notifications\" WHERE \"recipient_table\" = ? AND \"recipient_id\" = ? AND \"read_at\" IS NULL",
                vec![Value::Text(T::TABLE.to_owned()), Value::Integer(to.id().get())],
            )
            .await?;
        Ok(rows.first().map(|r| r.get(0)).unwrap_or(0))
    }

    /// Marks one of `to`'s entries read. Ids belonging to anyone else are ignored.
    pub async fn mark_notification_read<T: Record>(&self, to: &T, id: i64) -> Result<bool> {
        let rows = self
            .fetch_all(
                "UPDATE \"kiln_notifications\" SET \"read_at\" = COALESCE(\"read_at\", ?) \
                 WHERE \"id\" = ? AND \"recipient_table\" = ? AND \"recipient_id\" = ? RETURNING \"id\"",
                vec![
                    Value::Integer(now()),
                    Value::Integer(id),
                    Value::Text(T::TABLE.to_owned()),
                    Value::Integer(to.id().get()),
                ],
            )
            .await?;
        Ok(!rows.is_empty())
    }

    pub async fn mark_all_notifications_read<T: Record>(&self, to: &T) -> Result<usize> {
        let rows = self
            .fetch_all(
                "UPDATE \"kiln_notifications\" SET \"read_at\" = ? \
                 WHERE \"recipient_table\" = ? AND \"recipient_id\" = ? AND \"read_at\" IS NULL RETURNING \"id\"",
                vec![Value::Integer(now()), Value::Text(T::TABLE.to_owned()), Value::Integer(to.id().get())],
            )
            .await?;
        Ok(rows.len())
    }
}

fn row_to_notification(r: &sqlx::sqlite::SqliteRow) -> InboxItem {
    let at = |us: i64| Timestamp::from_jiff(jiff::Timestamp::from_microsecond(us).unwrap_or_default());
    InboxItem {
        id: r.get(0),
        kind: r.get(1),
        title: r.get(2),
        body: r.get(3),
        url: r.get(4),
        read: r.get::<Option<i64>, _>(5).is_some(),
        created_at: at(r.get(6)),
    }
}

/// Every notification type linked in, for `/kiln`.
pub(crate) fn kinds() -> Vec<&'static str> {
    let mut names: Vec<_> = inventory::iter::<NotificationDef>.into_iter().map(|d| d.name).collect();
    names.sort_unstable();
    names
}
