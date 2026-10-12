//! Views: a typed struct per page, rendered through a Jinja template.
//!
//! In debug builds templates are re-read on every render, so markup edits need no rebuild.
//! Missing variables and attributes are errors (strict undefined), never empty strings.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, OnceLock},
};

use axum::response::{Html, IntoResponse, Response};
use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior, Value, context, value::Kwargs};
use serde::{Serialize, ser::SerializeStruct};

use crate::{Result, routing};

/// Implemented by `#[derive(View)]`.
pub trait View: Serialize {
    /// Template file, relative to `ROOT`, e.g. `posts/show.html.jinja`.
    const TEMPLATE: &'static str;
    /// Absolute path of the app's `templates/` directory.
    const ROOT: &'static str;
    /// Layout file the page renders inside, e.g. `layouts/app.html.jinja`.
    const LAYOUT: Option<&'static str> = None;

    fn render(&self) -> Result<String> {
        render(Self::ROOT, Self::TEMPLATE, Self::LAYOUT, self)
    }
}

/// Renders `template`, then, when a layout is given, renders the layout with the same
/// context plus `content` (the page's HTML) and every top-level `{% set %}` from the page.
pub fn render<S: Serialize + ?Sized>(
    root: &'static str,
    template: &str,
    layout: Option<&str>,
    context: &S,
) -> Result<String> {
    let env = environment(root);
    let page = env.get_template(template)?;
    let Some(layout) = layout else {
        return Ok(page.render(context)?);
    };
    let captured = page.render_captured(context)?;
    let state = captured.state();
    let mut exports: BTreeMap<String, Value> =
        state.exports().into_iter().filter_map(|name| Some((name.to_owned(), state.lookup(name)?))).collect();
    exports.insert("content".to_owned(), Value::from_safe_string(captured.output().to_owned()));
    let layout_context = context! { ..Value::from(exports), ..Value::from_serialize(context) };
    Ok(env.get_template(layout)?.render(layout_context)?)
}

/// Presentation methods exposed to templates. Implemented by `#[part] impl Record { .. }`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` opts into parts but has no `#[part] impl {Self}` block",
    label = "declared with `#[record(.., parts)]`",
    note = "add `#[part] impl {Self} {{ .. }}` in `src/parts/`, or remove `parts` from `#[record(..)]`"
)]
pub trait Part {
    const NAMES: &'static [&'static str];

    fn serialize_parts<S: SerializeStruct>(&self, state: &mut S) -> std::result::Result<(), S::Error>;
}

/// Marks a record that declared `#[record(.., parts)]`. Never implemented by hand.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` has a `#[part]` block, but its record doesn't include parts",
    label = "templates would never see these methods",
    note = "add `parts` to `#[record(..)]` on `{Self}`"
)]
pub trait PartsEnabled {}

/// Compile-time check that no part method shares a name with a column.
#[doc(hidden)]
pub const fn assert_no_overlap(columns: &[&str], parts: &[&str]) {
    let mut i = 0;
    while i < columns.len() {
        let mut j = 0;
        while j < parts.len() {
            if str_eq(columns[i], parts[j]) {
                panic!("a `#[part]` method has the same name as a column; rename the method");
            }
            j += 1;
        }
        i += 1;
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[doc(hidden)]
pub fn respond<V: View>(view: &V) -> Response {
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => e.into_response(),
    }
}

fn environment(root: &'static str) -> Arc<Environment<'static>> {
    if cfg!(debug_assertions) {
        return Arc::new(build_environment(root));
    }
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Arc<Environment<'static>>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    cache.entry(root).or_insert_with(|| Arc::new(build_environment(root))).clone()
}

fn build_environment(root: &'static str) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader(root));
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Block tags (`{% if %}`, `{% set %}`) don't leave blank lines behind.
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_auto_escape_callback(|name| if name.contains(".html") { AutoEscape::Html } else { AutoEscape::None });
    env.add_function("path", path);
    env
}

/// `{{ path("posts::show", slug=post.slug) }}`: builds a URL from a registered route.
/// Unknown routes, missing captures and extra arguments are render errors.
fn path(name: String, kwargs: Kwargs) -> std::result::Result<minijinja::Value, minijinja::Error> {
    let route = routing::route_named(&name).ok_or_else(|| {
        let known: Vec<&str> = routing::routes().iter().map(|r| r.name()).collect();
        minijinja::Error::new(
            ErrorKind::InvalidOperation,
            format!("no route named `{name}`; known routes: {}", known.join(", ")),
        )
    })?;
    let mut url = String::new();
    for segment in route.path.split('/').skip(1) {
        url.push('/');
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(capture) => {
                let value: minijinja::Value = kwargs.get(capture)?;
                url.push_str(&routing::encode_segment(&value.to_string()));
            }
            None => url.push_str(segment),
        }
    }
    kwargs.assert_all_used()?;
    // Segments are percent-encoded above, so the URL is safe to emit unescaped.
    Ok(minijinja::Value::from_safe_string(url))
}
