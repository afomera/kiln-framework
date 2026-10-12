//! Macros for kiln. Every macro here expands to plain items that rust-analyzer can follow;
//! none of them introduce a DSL.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod action;
mod db_enum;
mod part;
mod record;
mod util;
mod view;

/// Derives a database record. The struct is the schema.
///
/// Generates, next to the struct:
/// - one typed column constant per field (`post::slug: Column<Post, String>`),
/// - one relation marker per `belongs_to`/`has_many` entry (`post::author`),
/// - a `{Name}Relations` trait with named accessors (`loaded_post.author()`),
/// - a `New{Name}` insert struct and `impl kiln::Record`.
///
/// Because columns and relations are emitted as module items, each record lives in its own
/// file named after it (`records/post.rs`).
#[proc_macro_derive(Record, attributes(record, index))]
pub fn derive_record(input: TokenStream) -> TokenStream {
    record::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::View` and `IntoResponse` for a page struct: `#[view(template = "posts/show")]`.
///
/// Pages render inside `templates/layouts/app.html.jinja` when it exists. Choose another
/// with `layout = "admin"`, or none with `layout = none`. The layout gets the view's fields,
/// `content` (the rendered page), and any top-level `{% set %}` from the page, such as `title`.
#[proc_macro_derive(View, attributes(view))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    view::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives storage for a unit-only enum as snake_case text with a CHECK constraint.
#[proc_macro_derive(DbEnum)]
pub fn derive_db_enum(input: TokenStream) -> TokenStream {
    db_enum::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Exposes a record's presentation methods to templates: `#[part] impl Post { .. }` in
/// `src/parts/post.rs`. Each method takes only `&self`; templates read it as a value
/// (`post.reading_time`). The record opts in with `#[record(.., parts)]`.
#[proc_macro_attribute]
pub fn part(attr: TokenStream, item: TokenStream) -> TokenStream {
    part::expand(attr.into(), item.into()).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Declares an HTTP endpoint: `#[action(GET "/posts/{slug}")]`.
///
/// Registers the route, and generates a typed `path(..)` helper in the same module with one
/// argument per `{capture}`. Each capture must match a `name: Param<T>` argument.
#[proc_macro_attribute]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    action::expand(attr.into(), item.into()).unwrap_or_else(syn::Error::into_compile_error).into()
}
