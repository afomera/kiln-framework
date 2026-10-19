//! Kiln: a full-stack Rust web framework where the compiler is the reviewer.
//!
//! M0 scope: records, typed queries, `Loaded<T, R>` preloading, `#[action]` routing with typed
//! path helpers, and minijinja-backed views. See the repository README for status.

extern crate self as kiln;

#[cfg(debug_assertions)]
mod agent;
mod app;
mod assets;
pub mod auth;
pub mod batch;
pub mod cache;
mod cli;
pub mod config;
mod db;
mod dev_reload;
mod devtools;
pub mod dialect;
mod error;
mod error_page;
mod files;
pub mod form;
pub mod http;
pub mod jobs;
pub mod join;
pub mod lint;
pub mod mail;
pub mod markdown;
mod migrate;
pub mod notify;
pub mod policy;
mod query;
mod query_params;
pub mod rate_limit;
mod realtime;
mod redirect;
pub mod rel;
pub mod rich_text;
mod routing;
pub mod schedule;
mod schema;
pub mod session;
mod sql;
pub mod storage;
pub mod testing;
pub mod turbo;
mod types;
pub mod view;
pub mod webhooks;
mod welcome;

pub use app::{App, Ctx};
pub use axum::Json;
/// Request headers, as an action argument: `headers: HeaderMap` (see `kiln::auth::bearer_matches`).
pub use axum::http::HeaderMap;
pub use cli::Cli;
pub use db::{Db, Role, Row};
pub use error::{Error, Result};
pub use files::Embedded;
pub use migrate::{
    ColumnSnapshot, Diff, Migration, RecordDef, Snapshot, TableSnapshot, diff, diff_for, load_migrations, records,
    write_migration,
};
pub use query::{Column, Delete, Expr, Insert, MinMax, Order, Query, Select, Subquery, Summable, Update};
pub use query_params::QueryParams;
pub use rate_limit::RateLimit;
pub use redirect::{Redirect, RequestHeaders, ViewOrRedirect};
pub use rel::{Append, BoxFuture, HasRel, HasRelation, Loaded, RelSet, Relation};
pub use rich_text::RichText;
pub use routing::{Param, PathArg, RouteDef, routes, routes_table};
pub use schema::{ColumnSchema, IndexSchema, TableSchema};
pub use session::{Current, Flash, SecretKey, Session};
pub use sql::{FromSqlRow, Sql};
pub use storage::{Attachment, Storage};
pub use turbo::{Turbo, TurboStream};
pub use types::{FromColumn, Id, SqlType, Timestamp, ToValue, Value};
pub use view::{Part, PartsEnabled, View};

/// Dates and times (the `jiff` crate), e.g. `kiln::time::SignedDuration`.
pub use jiff as time;

pub use batch::{Batch, BatchId};
pub use config::Secret;
pub use jobs::{Enqueue, Perform};
pub use kiln_macros::{
    Config, DbEnum, Form, Job, Mail, Notification, Record, View, action, embedded_files, part, template_filter,
    template_function, test,
};
pub use mail::{Mail, Preview};
pub use markdown::Markdown;
pub use notify::{Notify, Via};
pub use schedule::Schedule;

/// A database-backed entity. Derive it with `#[derive(Record)]`; the struct is the schema.
pub trait Record: Sized + Clone + Send + Sync + serde::Serialize + 'static {
    const TABLE: &'static str;
    const COLUMNS: &'static [&'static str];
    const HAS_UPDATED_AT: bool;

    /// Insertable fields: every column except `id`, `created_at` and `updated_at`.
    type New: Send;

    fn id(&self) -> Id<Self>;
    fn from_row(row: &Row) -> std::result::Result<Self, sqlx::Error>;
    fn new_values(new: Self::New) -> Vec<(&'static str, Value)>;
    fn schema() -> TableSchema;

    fn query() -> Query<Self> {
        Query::new()
    }

    fn find(id: Id<Self>) -> Query<Self> {
        Query::new().filter(Column::<Self, Id<Self>>::new("id").eq(id))
    }

    fn find_by(expr: Expr<Self>) -> Query<Self> {
        Query::new().filter(expr)
    }

    fn insert(new: Self::New) -> Insert<Self> {
        Insert::new(Self::new_values(new))
    }

    fn update(id: Id<Self>) -> Update<Self> {
        Update::new(id)
    }

    fn delete(id: Id<Self>) -> Delete<Self> {
        Delete::new(id)
    }
}

pub mod prelude {
    pub use kiln_macros::{Config, Form, Job, Notification};

    pub use crate::{
        App, Attachment, Batch, BatchId, Ctx, Current, Db, DbEnum, Enqueue, Error, Id, Json, Loaded, Mail, Markdown,
        Notify, Param, Perform, Preview, Query, QueryParams, Record, Redirect, Result, RichText, Schedule, Secret,
        Session, Timestamp, Turbo, TurboStream, Via, View, ViewOrRedirect, action,
        form::{Form, FormState, OrRender, Valid},
        part,
        policy::{Allows, Authorize, Authorized},
    };
    pub use crate::{HeaderMap, RateLimit};
}

#[doc(hidden)]
pub mod __private {
    pub use axum;
    pub use axum::response::{IntoResponse, Response};
    pub use inventory;
    pub use serde;
    pub use sqlx;
    pub use tokio;

    pub use crate::db::Row;
    pub use crate::routing::{RawParams, RequiredScope, action_arg, last_action_arg};
    pub use crate::view::assert_no_overlap;

    pub type Router = axum::Router<crate::Ctx>;
}
