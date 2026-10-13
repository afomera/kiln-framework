//! Kiln: a full-stack Rust web framework where the compiler is the reviewer.
//!
//! M0 scope: records, typed queries, `Loaded<T, R>` preloading, `#[action]` routing with typed
//! path helpers, and minijinja-backed views. See the repository README for status.

extern crate self as kiln;

mod app;
mod assets;
pub mod auth;
mod cli;
pub mod config;
mod db;
mod error;
mod error_page;
pub mod form;
pub mod jobs;
pub mod mail;
mod migrate;
pub mod policy;
mod query;
mod realtime;
mod redirect;
pub mod rel;
mod routing;
mod schema;
pub mod session;
pub mod testing;
pub mod turbo;
mod types;
pub mod view;

pub use app::{App, Ctx};
pub use axum::Json;
pub use cli::Cli;
pub use db::Db;
pub use error::{Error, Result};
pub use migrate::{
    ColumnSnapshot, Diff, Migration, RecordDef, Snapshot, TableSnapshot, diff, load_migrations, records,
    write_migration,
};
pub use query::{Column, Delete, Expr, Insert, Order, Query, Update};
pub use redirect::{Redirect, ViewOrRedirect};
pub use rel::{Append, BoxFuture, HasRel, HasRelation, Loaded, RelSet, Relation};
pub use routing::{Param, PathArg, RouteDef, routes, routes_table};
pub use schema::{ColumnSchema, TableSchema};
pub use session::{Current, Flash, SecretKey, Session};
pub use turbo::{Turbo, TurboStream};
pub use types::{FromColumn, Id, SqlType, Timestamp, ToValue, Value};
pub use view::{Part, PartsEnabled, View};

/// Dates and times (the `jiff` crate), e.g. `kiln::time::SignedDuration`.
pub use jiff as time;

pub use config::Secret;
pub use jobs::{Enqueue, Perform};
pub use mail::Mail;
pub use kiln_macros::{Config, DbEnum, Form, Job, Mail, Record, View, action, part, test};

use sqlx::sqlite::SqliteRow;

/// A database-backed entity. Derive it with `#[derive(Record)]`; the struct is the schema.
pub trait Record: Sized + Clone + Send + Sync + serde::Serialize + 'static {
    const TABLE: &'static str;
    const COLUMNS: &'static [&'static str];
    const HAS_UPDATED_AT: bool;

    /// Insertable fields: every column except `id`, `created_at` and `updated_at`.
    type New: Send;

    fn id(&self) -> Id<Self>;
    fn from_row(row: &SqliteRow) -> std::result::Result<Self, sqlx::Error>;
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
    pub use kiln_macros::{Config, Form, Job};

    pub use crate::{
        App, Ctx, Current, Db, DbEnum, Enqueue, Error, Id, Json, Loaded, Mail, Param, Perform, Query, Record, Redirect, Result, Secret, Session,
        Timestamp, Turbo, TurboStream, View, ViewOrRedirect, action,
        form::{Form, FormState, OrRender, Valid},
        part,
        policy::{Allows, Authorize, Authorized},
    };
}

#[doc(hidden)]
pub mod __private {
    pub use axum;
    pub use axum::response::{IntoResponse, Response};
    pub use inventory;
    pub use serde;
    pub use sqlx;
    pub use sqlx::sqlite::SqliteRow;
    pub use tokio;

    pub use crate::routing::RawParams;
    pub use crate::view::assert_no_overlap;

    pub type Router = axum::Router<crate::Ctx>;
}
