//! Kiln: a full-stack Rust web framework where the compiler is the reviewer.
//!
//! M0 scope: records, typed queries, `Loaded<T, R>` preloading, `#[action]` routing with typed
//! path helpers, and minijinja-backed views. See the repository README for status.

extern crate self as kiln;

mod app;
mod db;
mod error;
mod error_page;
mod query;
pub mod rel;
mod routing;
mod schema;
pub mod testing;
mod types;
pub mod view;

pub use app::{App, Ctx};
pub use axum::response::Redirect;
pub use db::Db;
pub use error::{Error, Result};
pub use query::{Column, Expr, Insert, Order, Query, Update};
pub use rel::{Append, BoxFuture, HasRel, HasRelation, Loaded, RelSet, Relation};
pub use routing::{Param, PathArg, RouteDef, routes, routes_table};
pub use schema::{ColumnSchema, TableSchema};
pub use types::{FromColumn, Id, SqlType, Timestamp, ToValue, Value};
pub use view::{Part, PartsEnabled, View};

pub use kiln_macros::{DbEnum, Record, View, action, part};

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
}

pub mod prelude {
    pub use crate::{
        App, Ctx, Db, DbEnum, Error, Id, Loaded, Param, Query, Record, Redirect, Result, Timestamp, View, action, part,
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

    pub use crate::routing::RawParams;
    pub use crate::view::assert_no_overlap;

    pub type Router = axum::Router<crate::Ctx>;
}
