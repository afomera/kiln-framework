//! Macros for kiln. Every macro here expands to plain items that rust-analyzer can follow;
//! none of them introduce a DSL.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod action;
mod config;
mod db_enum;
mod embed;
mod form;
mod job;
mod lint;
mod mail;
mod notification;
mod part;
mod record;
mod test;
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
/// `#[hidden]` on a field (a password digest) keeps it out of serialization: templates,
/// JSON and broadcasts never see it.
///
/// Because columns and relations are emitted as module items, each record lives in its own
/// file named after it (`records/post.rs`).
#[proc_macro_derive(Record, attributes(record, index, hidden))]
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
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident.clone();
    view::expand(input).unwrap_or_else(|e| with_stub(e, view::stub(&name))).into()
}

/// Derives storage for a unit-only enum as snake_case text with a CHECK constraint.
#[proc_macro_derive(DbEnum)]
pub fn derive_db_enum(input: TokenStream) -> TokenStream {
    db_enum::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::config::Config`: each field read from its environment variable
/// (`smtp_url` → `SMTP_URL`), then the encrypted secrets file, then `default`, then (outside
/// production) `dev_default`. `Option<T>` fields are optional.
#[proc_macro_derive(Config, attributes(config))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    config::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::jobs::JobInfo` and registers the job with workers:
/// `#[derive(Job, Serialize, Deserialize)] #[job(queue = "mail", attempts = 5)]`.
/// Pair it with `impl Perform for MyJob`.
#[proc_macro_derive(Job, attributes(job))]
pub fn derive_job(input: TokenStream) -> TokenStream {
    job::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::Mail` for an email:
/// `#[derive(Mail, Serialize)] #[mail(template = "mail/welcome", subject = "Welcome, {{ user.name }}")]`.
/// Needs a `to: String` field. The body is `templates/mail/welcome.html.jinja` (checked at
/// compile time); `welcome.txt.jinja` beside it adds a plain-text part.
#[proc_macro_derive(Mail, attributes(mail))]
pub fn derive_mail(input: TokenStream) -> TokenStream {
    mail::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::notify::NotificationInfo` and registers the notification for delivery:
/// `#[derive(Notification, Serialize, Deserialize)]`, optionally with
/// `#[notification(description = "Someone comments on your post", optional(mail, inbox))]`.
/// Pair it with `impl Notify for ..`.
#[proc_macro_derive(Notification, attributes(notification))]
pub fn derive_notification(input: TokenStream) -> TokenStream {
    notification::expand(parse_macro_input!(input as DeriveInput)).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Derives `kiln::form::FormInput`: parsing and validation of submitted forms.
///
/// Field rules: `#[validate(length(min = 1, max = 200))]`, `range(min = 0, max = 10)`,
/// `email`, `required` (for `Option` fields). Optional fields are `Option<T>`; checkboxes
/// are `bool`. Rendering: `#[form(textarea)]`, `#[form(password)]` (never re-rendered), `#[form(label = "Starts")]`,
/// `#[form(hint = "Shown under the input")]`, `#[form(placeholder = "..")]`.
#[proc_macro_derive(Form, attributes(validate, form))]
pub fn derive_form(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident.clone();
    form::expand(input).unwrap_or_else(|e| with_stub(e, form::stub(&name))).into()
}

/// The error, plus a stand-in impl so the compiler reports only the real mistake and not
/// a cascade of "trait not implemented" errors from code that uses the type.
fn with_stub(error: syn::Error, stub: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let error = error.into_compile_error();
    quote::quote! { #error #stub }
}

/// A test against a fresh in-memory app with the crate's migrations applied:
/// `#[kiln::test] async fn creates_post(app: TestApp) -> kiln::Result { .. }`.
/// The app's templates, assets, `public/` files, migrations, schema snapshot and encrypted
/// secrets, compiled into release builds: `kiln::Cli::new(..).files(kiln::embedded_files!())`.
#[proc_macro]
pub fn embedded_files(input: TokenStream) -> TokenStream {
    embed::expand(input.into()).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test::expand(attr.into(), item.into()).unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Registers one of the app's own `kiln check` rules: `#[lint(code = "A0001")]` on
/// `fn rule(file: &File, report: &mut Report)` in `src/lints/`. `level = "warning"` makes
/// its findings warnings. The doc comment is what `kiln explain A0001` prints.
#[proc_macro_attribute]
pub fn lint(attr: TokenStream, item: TokenStream) -> TokenStream {
    lint::expand(attr.into(), item.into()).unwrap_or_else(syn::Error::into_compile_error).into()
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
