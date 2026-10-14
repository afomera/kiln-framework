//! Views: a typed struct per page, rendered through a Jinja template.
//!
//! In debug builds templates are re-read on every render, so markup edits need no rebuild.
//! Missing variables and attributes are errors (strict undefined), never empty strings.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior, Value, context, value::Kwargs};
use serde::{Serialize, ser::SerializeStruct};

use crate::{Result, routing, session};

/// Implemented by `#[derive(View)]`.
pub trait View: Serialize {
    /// Template file, relative to `ROOT`, e.g. `posts/show.html.jinja`.
    const TEMPLATE: &'static str;
    /// Absolute path of the app's `templates/` directory.
    const ROOT: &'static str;
    /// Layout file the page renders inside, e.g. `layouts/app.html.jinja`.
    const LAYOUT: Option<&'static str> = None;

    /// Renders now. Actions don't call this: returning the view lets kiln render it after
    /// the action, once the `current_*` values its templates use are loaded.
    fn render(&self) -> Result<String> {
        PendingRender::of(self).render()
    }
}

/// A view whose data is captured but not yet rendered. Kiln renders it as the last step
/// of a request, so templates (and layouts) can use request-wide values like
/// `current_user` that have to be loaded from the database first.
#[derive(Debug, Clone)]
pub struct PendingRender {
    root: &'static str,
    template: &'static str,
    layout: Option<&'static str>,
    context: Value,
}

impl PendingRender {
    pub fn of<V: View + ?Sized>(view: &V) -> Self {
        Self { root: V::ROOT, template: V::TEMPLATE, layout: V::LAYOUT, context: Value::from_serialize(view) }
    }

    pub fn render(&self) -> Result<String> {
        render_value(self.root, self.template, self.layout, self.context.clone())
    }

    /// Just the page: for Turbo Frame requests and stream content.
    pub fn render_without_layout(&self) -> Result<String> {
        render_value(self.root, self.template, None, self.context.clone())
    }

    /// Variables read by the page, its layout, and everything they include or extend.
    pub(crate) fn variables(&self) -> Result<HashSet<String>> {
        let env = environment(self.root);
        let mut seen = HashSet::new();
        let mut variables = HashSet::new();
        collect_variables(&env, self.template, &mut seen, &mut variables)?;
        if let Some(layout) = self.layout {
            collect_variables(&env, layout, &mut seen, &mut variables)?;
        }
        Ok(variables)
    }
}

fn collect_variables(
    env: &Environment<'static>,
    name: &str,
    seen: &mut HashSet<String>,
    variables: &mut HashSet<String>,
) -> Result<()> {
    if !seen.insert(name.to_owned()) {
        return Ok(());
    }
    let template = env.get_template(name)?;
    variables.extend(template.undeclared_variables(false));
    let source = template.source().to_owned();
    for keyword in ["include", "extends", "import", "from"] {
        for chunk in source.split(&format!("{keyword} \"")).skip(1) {
            if let Some(other) = chunk.split('"').next() {
                collect_variables(env, other, seen, variables)?;
            }
        }
    }
    Ok(())
}

/// Renders `template`, then, when a layout is given, renders the layout with the same
/// context plus `content` (the page's HTML) and every top-level `{% set %}` from the page.
pub fn render<S: Serialize + ?Sized>(
    root: &'static str,
    template: &str,
    layout: Option<&str>,
    context: &S,
) -> Result<String> {
    render_value(root, template, layout, Value::from_serialize(context))
}

/// An email's HTML body. Filters that link page assets (`rich_text`) leave them out, since
/// mail clients don't load them.
pub(crate) fn render_mail<S: Serialize + ?Sized>(
    root: &'static str,
    template: &str,
    layout: Option<&str>,
    context: &S,
) -> Result<String> {
    render_value(root, template, layout, context! { __kiln_mail => true, ..Value::from_serialize(context) })
}

/// Renders a template string (a mail subject) with `context`.
pub(crate) fn render_str<S: Serialize + ?Sized>(root: &'static str, source: &str, context: &S) -> Result<String> {
    Ok(environment(root).render_str(source, context)?)
}

fn render_value(root: &'static str, template: &str, layout: Option<&str>, context: Value) -> Result<String> {
    let env = environment(root);
    let page = env.get_template(template)?;
    // Request-wide values (`flash`, `current_*`); the view's own fields win on a name clash.
    let scope = session::current_scope();
    let mut globals: BTreeMap<String, Value> = scope.as_ref().map(|s| s.cache.template_values()).unwrap_or_default();
    globals.insert("flash".to_owned(), Value::from_serialize(scope.map(|s| s.flash.0).unwrap_or_default()));
    let context = context! { ..context, ..Value::from(globals) };
    let Some(layout) = layout else {
        return Ok(page.render(&context)?);
    };
    let captured = page.render_captured(&context)?;
    let state = captured.state();
    let mut exports: BTreeMap<String, Value> =
        state.exports().into_iter().filter_map(|name| Some((name.to_owned(), state.lookup(name)?))).collect();
    exports.insert("content".to_owned(), Value::from_safe_string(captured.output().to_owned()));
    let layout_context = context! { ..Value::from(exports), ..context };
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

/// The response for a view: rendered by kiln after the action (see [`PendingRender`]).
#[doc(hidden)]
pub fn respond<V: View>(view: &V) -> Response {
    let mut response = StatusCode::OK.into_response();
    response.extensions_mut().insert(PendingRender::of(view));
    response
}

fn environment(root: &'static str) -> Arc<Environment<'static>> {
    if cfg!(debug_assertions) {
        return Arc::new(build_environment(root));
    }
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Arc<Environment<'static>>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    cache.entry(root).or_insert_with(|| Arc::new(build_environment(root))).clone()
}

/// For `kiln check`: each template's variables that it doesn't define itself
/// (`post.title`, `comments`), and the names every template can use without a view field.
#[cfg(debug_assertions)]
pub(crate) fn template_variables(root: &'static str) -> serde_json::Value {
    let env = build_environment(root);
    let mut globals: Vec<String> = env.globals().map(|(name, _)| name.to_owned()).collect();
    // Set per render or by the template engine itself.
    globals.extend(["flash", "loop", "self", "super", "caller", "varargs", "kwargs"].map(str::to_owned));
    let mut templates = serde_json::Map::new();
    for path in crate::files::walk(std::path::Path::new(root)) {
        let Some(name) =
            path.strip_prefix(root).ok().map(|p| p.to_string_lossy().trim_start_matches('/').replace('\\', "/"))
        else {
            continue;
        };
        if !name.ends_with(".jinja") {
            continue;
        }
        let Ok(template) = env.get_template(&name) else { continue };
        let mut used: Vec<String> = template.undeclared_variables(true).into_iter().collect();
        used.sort();
        templates.insert(name, serde_json::json!(used));
    }
    serde_json::json!({ "globals": globals, "templates": templates })
}

fn build_environment(root: &'static str) -> Environment<'static> {
    let mut env = Environment::new();
    // Views know their templates by the path they were compiled with; a deployed binary
    // finds them under `KILN_ROOT` instead.
    let embedded = crate::files::embedded() && root.ends_with("/templates");
    let templates = match crate::config::root_override() {
        Some(deployed) if root.ends_with("/templates") => deployed.join("templates"),
        _ => std::path::PathBuf::from(root),
    };
    let assets = match crate::config::root().filter(|_| embedded) {
        // Compiled-in templates: assets are compiled in beside them, under the app's root.
        Some(app_root) => crate::assets::Assets::at(&app_root),
        None => crate::assets::Assets::beside_templates(&templates.to_string_lossy()),
    };
    if embedded {
        env.set_loader(|name| Ok(crate::files::template(name)));
    } else {
        env.set_loader(minijinja::path_loader(templates));
    }
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Block tags (`{% if %}`, `{% set %}`) don't leave blank lines behind.
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_auto_escape_callback(|name| if name.contains(".html") { AutoEscape::Html } else { AutoEscape::None });
    env.add_function("path", path);
    // Absolute links, for emails: `{{ url(path("posts::show", post.slug)) }}`.
    env.add_function("url", |path: String| {
        let url = crate::mail::absolute_url(&path);
        Value::from_safe_string(url.replace('"', "%22").replace('<', "%3C").replace('>', "%3E"))
    });
    env.add_function("csrf_field", csrf_field);
    env.add_function("csrf_token", csrf_token);
    env.add_function("csrf_meta", csrf_meta);
    env.add_function("turbo_stream_from", turbo_stream_from);
    let lookup = assets.clone();
    // Asset URLs are safe to emit unescaped (like `path()`), so `/` isn't turned into `&#x2f;`.
    env.add_function("asset", move |name: String| -> std::result::Result<Value, minijinja::Error> {
        let url = lookup.url(&name).ok_or_else(|| {
            minijinja::Error::new(ErrorKind::InvalidOperation, format!("asset `{name}` not found in assets/"))
        })?;
        Ok(Value::from_safe_string(url.replace('"', "%22").replace('<', "%3C").replace('>', "%3E")))
    });
    env.add_function("javascript_importmap", move || javascript_importmap(&assets));
    env.add_filter("datetime", datetime);
    // `{{ post.body | rich_text }}`: rich text as HTML, sanitized again on the way out.
    env.add_filter("rich_text", |state: &minijinja::State, html: String| -> Value {
        let clean = crate::RichText::new(&html);
        if clean.is_empty() {
            return Value::from_safe_string(String::new());
        }
        let mail = state.lookup("__kiln_mail").is_some_and(|v| v.is_true());
        // Attachments show only with links Kiln signed; mail needs them absolute.
        let html = crate::rich_text::render_attachments(clean.as_html(), &|url| {
            crate::storage::verified_file_link(url).map(|url| if mail { crate::mail::absolute_url(&url) } else { url })
        });
        if mail {
            // Email: no stylesheet or script to load; the mail layout styles the content.
            return Value::from_safe_string(format!("<div class=\"kiln-rich-text-content\">{html}</div>"));
        }
        // Code blocks are highlighted in the browser, so only pages with one load the script.
        let highlight = if clean.contains("<pre") {
            "<script type=\"module\">import \"kiln/rich-text-display\"</script>"
        } else {
            ""
        };
        Value::from_safe_string(format!(
            "<link rel=\"stylesheet\" href=\"{}\"><div class=\"lexxy-content kiln-rich-text-content\">{}</div>{highlight}",
            crate::assets::rich_text_css_url(),
            html
        ))
    });
    // `{{ post.cover | variant(width=400) }}`, `variant(width=200, height=200, crop=true)`:
    // a resized copy's URL (made on first request, then kept).
    env.add_filter("variant", |file: Value, kwargs: Kwargs| -> std::result::Result<Value, minijinja::Error> {
        let width: Option<u32> = kwargs.get("width")?;
        let height: Option<u32> = kwargs.get("height")?;
        let crop: Option<bool> = kwargs.get("crop")?;
        kwargs.assert_all_used()?;
        if file.is_none() || file.is_undefined() {
            return Ok(Value::from(""));
        }
        let url = file.get_attr("url")?.as_str().map(str::to_owned).unwrap_or_default();
        let variant = crate::storage::Variant {
            width: width.unwrap_or(0),
            height: height.unwrap_or(0),
            crop: crop.unwrap_or(false),
        };
        let resized = crate::storage::variant_for_url(&url, variant).ok_or_else(|| {
            minijinja::Error::new(
                ErrorKind::InvalidOperation,
                "variant() takes a stored file, e.g. `post.cover | variant(width=400)`",
            )
        })?;
        Ok(Value::from_safe_string(resized))
    });
    // `{{ issue.body | markdown }}`: GitHub-flavored markdown, sanitized and highlighted.
    env.add_filter("markdown", |state: &minijinja::State, source: Option<String>| -> Value {
        let source = source.unwrap_or_default();
        if source.trim().is_empty() {
            return Value::from_safe_string(String::new());
        }
        let html = crate::markdown::to_html(&source);
        // Email clients load no stylesheets; the mail layout styles the content.
        let link = if state.lookup("__kiln_mail").is_some_and(|v| v.is_true()) { "" } else { &markdown_css() };
        Value::from_safe_string(format!("{link}<div class=\"kiln-markdown\">{html}</div>"))
    });
    // `{{ blob.text | highlight(blob.path) }}`: a whole file, highlighted by its name.
    env.add_filter("highlight", |code: Option<String>, filename: String| -> Value {
        let html = crate::markdown::highlight(&code.unwrap_or_default(), &filename);
        Value::from_safe_string(format!("{}{html}", markdown_css()))
    });
    // `{% for line in blob.text | highlight_lines(blob.path) %}`: one safe string per line.
    env.add_filter("highlight_lines", |code: Option<String>, filename: String| -> Vec<Value> {
        crate::markdown::highlight_lines(&code.unwrap_or_default(), &filename)
            .into_iter()
            .map(Value::from_safe_string)
            .collect()
    });
    // The stylesheet `highlight_lines` output needs (the other filters link it themselves).
    env.add_function("markdown_css", || Value::from_safe_string(markdown_css()));
    // `{{ post.body | plain_text }}`: rich text as words, for excerpts and mail.
    env.add_filter("plain_text", |html: String| crate::RichText::new(&html).to_plain_text());
    env.add_function("form_start", forms::form_start);
    env.add_function("form_end", forms::form_end);
    env.add_function("button_to", forms::button_to);
    env.add_function("fields", forms::fields);
    env.add_function("field", forms::field);
    env
}

fn markdown_css() -> String {
    format!("<link rel=\"stylesheet\" href=\"{}\">", crate::markdown::stylesheet_url())
}

/// `{{ csrf_token() }}`: the session's CSRF token, for `X-CSRF-Token` headers in JS.
fn csrf_token() -> std::result::Result<String, minijinja::Error> {
    session::current_scope().map(|scope| scope.session.csrf_token()).ok_or_else(|| {
        minijinja::Error::new(
            ErrorKind::InvalidOperation,
            "csrf_token() needs a request; render the view from an action",
        )
    })
}

/// `{{ post.published_at | datetime }}` → `Oct 20, 2026 18:00 UTC`. Takes an optional
/// strftime format: `{{ starts_at | datetime("%Y-%m-%d") }}`. Missing values render empty.
fn datetime(value: Value, format: Option<String>) -> std::result::Result<String, minijinja::Error> {
    if value.is_none() || value.is_undefined() {
        return Ok(String::new());
    }
    let ts: jiff::Timestamp = value.to_string().parse().map_err(|e| {
        minijinja::Error::new(ErrorKind::InvalidOperation, format!("datetime: `{value}` isn't a timestamp ({e})"))
    })?;
    Ok(ts.strftime(format.as_deref().unwrap_or("%b %-d, %Y %H:%M UTC")).to_string())
}

/// Escapes text for an HTML attribute value.
pub(crate) fn escape_attribute(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `{{ turbo_stream_from("post:" ~ post.id) }}`: subscribes the page to broadcasts on a
/// stream. The name is signed, so pages can only listen to streams they were rendered with.
fn turbo_stream_from(stream: String) -> std::result::Result<Value, minijinja::Error> {
    let key = session::current_scope().and_then(|scope| scope.key).ok_or_else(|| {
        minijinja::Error::new(
            ErrorKind::InvalidOperation,
            "turbo_stream_from() needs a request; render the view from an action",
        )
    })?;
    Ok(Value::from_safe_string(crate::realtime::source_tag(&key, &stream)))
}

/// `{{ csrf_meta() }}`: in the layout's `<head>`, so Turbo and other JS send the token as
/// an `X-CSRF-Token` header.
fn csrf_meta() -> std::result::Result<Value, minijinja::Error> {
    let token = csrf_token()?;
    Ok(Value::from_safe_string(format!(
        "<meta name=\"csrf-param\" content=\"_csrf\">\n<meta name=\"csrf-token\" content=\"{token}\">"
    )))
}

/// `{{ javascript_importmap() }}`: in the layout's `<head>`. Maps `@hotwired/turbo` and
/// every `assets/**/*.js` file, then loads Turbo and `assets/application.js`.
fn javascript_importmap(assets: &crate::assets::Assets) -> Value {
    let imports = assets.importmap();
    let json = serde_json::json!({ "imports": imports }).to_string().replace("</", "<\\/");
    let mut html = format!("<script type=\"importmap\">{json}</script>\n");
    html.push_str(&format!("<link rel=\"modulepreload\" href=\"{}\">\n", crate::assets::turbo_url()));
    html.push_str(&format!("<link rel=\"modulepreload\" href=\"{}\">\n", crate::assets::confirm_url()));
    html.push_str("<script type=\"module\">import \"@hotwired/turbo\"; import \"kiln/confirm\";");
    if crate::dev_reload::enabled() {
        html.push_str(" import \"kiln/dev-reload\";");
    }
    if assets.has_application_js() {
        html.push_str(" import \"application\";");
    }
    html.push_str("</script>");
    Value::from_safe_string(html)
}

/// `{{ csrf_field() }}`: the hidden input every POST form needs.
fn csrf_field() -> std::result::Result<Value, minijinja::Error> {
    let token = csrf_token()?;
    Ok(Value::from_safe_string(format!("<input type=\"hidden\" name=\"_csrf\" value=\"{token}\">")))
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
        match routing::capture_name(segment) {
            // A catch-all keeps the slashes in its value: `path="src/lib.rs"`.
            Some(capture) if segment.starts_with("{*") => {
                let value: minijinja::Value = kwargs.get(capture)?;
                url.push_str(&routing::encode_segments(&value.to_string()));
            }
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

/// Form helpers. Markup comes from the form's `#[derive(Form)]` metadata, so inputs carry
/// the same constraints as the validation rules. Apps restyle every field by adding
/// `templates/kiln/field.html.jinja`, which receives `field`, `label`, `input`, `hint`
/// and `placeholder`.
mod forms {
    use std::fmt::Write as _;

    use minijinja::{Error, ErrorKind, State, Value, context, value::Kwargs};

    use super::csrf_token;

    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&#x27;"),
                c => out.push(c),
            }
        }
        out
    }

    /// `{{ form_start(path("posts::update", slug=post.slug), method="patch") }}`: the
    /// `<form>` tag, its CSRF token, and the `_method` override for PATCH/PUT/DELETE.
    /// `confirm="Delete this post?"` asks first (Kiln's dialog, or yours: `kiln/confirm`);
    /// `confirm_button="Delete"` labels its button and `tone="danger"` marks it destructive.
    pub fn form_start(action: String, kwargs: Kwargs) -> Result<Value, Error> {
        let method: Option<String> = kwargs.get("method")?;
        let class: Option<String> = kwargs.get("class")?;
        // `multipart=true` for forms with file fields.
        let multipart: Option<bool> = kwargs.get("multipart")?;
        let mut extra = confirm_attributes(&kwargs)?;
        if multipart == Some(true) {
            extra.push_str(" enctype=\"multipart/form-data\"");
        }
        kwargs.assert_all_used()?;
        Ok(Value::from_safe_string(form_open(&action, method.as_deref(), class.as_deref(), &extra)?))
    }

    /// `<form ..>` with the CSRF token and method override.
    fn form_open(action: &str, method: Option<&str>, class: Option<&str>, extra: &str) -> Result<String, Error> {
        let method = method.unwrap_or("post").to_ascii_lowercase();
        let class = class.map(|c| format!(" class=\"{}\"", esc(c))).unwrap_or_default();
        if method == "get" {
            return Ok(format!("<form method=\"get\" action=\"{}\"{class}{extra}>", esc(action)));
        }
        if !matches!(method.as_str(), "post" | "patch" | "put" | "delete") {
            return Err(Error::new(ErrorKind::InvalidOperation, format!("form_start: unknown method `{method}`")));
        }
        let mut html = format!(
            "<form method=\"post\" action=\"{}\"{class}{extra}>\n<input type=\"hidden\" name=\"_csrf\" value=\"{}\">",
            esc(action),
            esc(&csrf_token()?)
        );
        if method != "post" {
            let _ =
                write!(html, "\n<input type=\"hidden\" name=\"_method\" value=\"{}\">", method.to_ascii_uppercase());
        }
        Ok(html)
    }

    /// `data-turbo-confirm` and friends from `confirm=`, `confirm_button=`, `tone=`.
    fn confirm_attributes(kwargs: &Kwargs) -> Result<String, Error> {
        let mut attrs = String::new();
        if let Some(message) = kwargs.get::<Option<String>>("confirm")? {
            let _ = write!(attrs, " data-turbo-confirm=\"{}\"", esc(&message));
        }
        if let Some(label) = kwargs.get::<Option<String>>("confirm_button")? {
            let _ = write!(attrs, " data-confirm-button=\"{}\"", esc(&label));
        }
        if let Some(tone) = kwargs.get::<Option<String>>("tone")? {
            let _ = write!(attrs, " data-confirm-tone=\"{}\"", esc(&tone));
        }
        Ok(attrs)
    }

    /// `{{ button_to("Delete", path("posts::destroy", slug=post.slug), method="delete",
    /// confirm="Delete this post?", tone="danger") }}`: a one-button form, CSRF included.
    /// `class=` goes on the form (always also `button-to`), `button_class=` on the button.
    pub fn button_to(label: String, action: String, kwargs: Kwargs) -> Result<Value, Error> {
        let method: Option<String> = kwargs.get("method")?;
        let class: Option<String> = kwargs.get("class")?;
        let button_class: Option<String> = kwargs.get("button_class")?;
        let confirm = confirm_attributes(&kwargs)?;
        kwargs.assert_all_used()?;
        let class = match class {
            Some(class) => format!("button-to {class}"),
            None => "button-to".to_owned(),
        };
        let open = form_open(&action, Some(method.as_deref().unwrap_or("post")), Some(&class), &confirm)?;
        let button_class = button_class.map(|c| format!(" class=\"{}\"", esc(&c))).unwrap_or_default();
        Ok(Value::from_safe_string(format!(
            "{open}\n<button type=\"submit\"{button_class}>{}</button></form>",
            esc(&label)
        )))
    }

    pub fn form_end() -> Value {
        Value::from_safe_string("</form>".into())
    }

    /// `{{ fields(form) }}`: form-wide errors, then every field in declaration order.
    pub fn fields(state: &State, form: Value) -> Result<Value, Error> {
        let names = form.get_attr("_fields")?;
        if names.is_undefined() {
            return Err(Error::new(ErrorKind::InvalidOperation, "fields() takes a form, e.g. `fields(form)`"));
        }
        let mut html = String::new();
        let base: Vec<String> = form.get_attr("_errors")?.try_iter()?.map(|e| e.to_string()).collect();
        if !base.is_empty() {
            html.push_str("<div class=\"form-errors\" role=\"alert\">");
            for message in base {
                let _ = write!(html, "<p>{}</p>", esc(&message));
            }
            html.push_str("</div>\n");
        }
        for name in names.try_iter()? {
            let field = form.get_attr(&name.to_string())?;
            html.push_str(&render(state, &field, Overrides::default())?);
            html.push('\n');
        }
        Ok(Value::from_safe_string(html))
    }

    /// `{{ field(form.title, label="Headline", hint="Shown on the home page") }}`.
    /// Also `as="textarea"` and `placeholder="..."`.
    pub fn field(state: &State, field: Value, kwargs: Kwargs) -> Result<Value, Error> {
        let overrides = Overrides {
            label: kwargs.get("label")?,
            input: kwargs.get("as")?,
            hint: kwargs.get("hint")?,
            placeholder: kwargs.get("placeholder")?,
        };
        kwargs.assert_all_used()?;
        if field.get_attr("input")?.is_undefined() {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "field() takes a form field, e.g. `field(form.title)`",
            ));
        }
        Ok(Value::from_safe_string(render(state, &field, overrides)?))
    }

    #[derive(Default)]
    struct Overrides {
        label: Option<String>,
        input: Option<String>,
        hint: Option<String>,
        placeholder: Option<String>,
    }

    fn text(field: &Value, key: &str) -> Result<String, Error> {
        let value = field.get_attr(key)?;
        Ok(if value.is_none() || value.is_undefined() { String::new() } else { value.to_string() })
    }

    /// `200.0` → `200`.
    fn number(value: &Value) -> Option<String> {
        if value.is_none() || value.is_undefined() {
            return None;
        }
        let n = f64::try_from(value.clone()).ok()?;
        Some(if n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() })
    }

    fn render(state: &State, field: &Value, o: Overrides) -> Result<String, Error> {
        let name = text(field, "name")?;
        let label = o.label.unwrap_or(text(field, "label")?);
        let input = o.input.unwrap_or(text(field, "input")?);
        let spec_text = |key: &str| text(field, key).map(|t| Some(t).filter(|t| !t.is_empty()));
        // Times are read as UTC (see `FormField for Timestamp`), so say so.
        let default_hint = (input == "datetime-local").then(|| "Time in UTC.".to_owned());
        let o = Overrides {
            label: None,
            input: None,
            hint: o.hint.or(spec_text("hint")?).or(default_hint),
            placeholder: o.placeholder.or(spec_text("placeholder")?),
        };

        // An app-wide field template replaces the built-in markup.
        if let Ok(custom) = state.env().get_template("kiln/field.html.jinja") {
            return custom.render(context! {
                field => field, label => label, input => input, hint => o.hint, placeholder => o.placeholder,
            });
        }

        let value = text(field, "value")?;
        let errors: Vec<String> = field.get_attr("errors")?.try_iter()?.map(|e| e.to_string()).collect();
        let required = field.get_attr("required")?.is_true();
        let id = esc(&name);
        let mut attrs = format!("id=\"{id}\" name=\"{id}\"");
        if required {
            attrs.push_str(" required");
        }
        if !errors.is_empty() {
            let _ = write!(attrs, " aria-invalid=\"true\" aria-describedby=\"{id}-error\"");
        }
        if let Some(placeholder) = &o.placeholder {
            let _ = write!(attrs, " placeholder=\"{}\"", esc(placeholder));
        }
        // Password managers rely on these to fill and save the right thing.
        let default_autocomplete = match input.as_str() {
            "password" => Some("new-password".to_owned()),
            "email" => Some("email".to_owned()),
            _ => None,
        };
        if let Some(autocomplete) = spec_text("autocomplete")?.or(default_autocomplete) {
            let _ = write!(attrs, " autocomplete=\"{}\"", esc(&autocomplete));
        }
        let lengths = |attrs: &mut String| -> Result<(), Error> {
            if let Some(min) = number(&field.get_attr("min_length")?).filter(|m| m != "1" && m != "0") {
                let _ = write!(attrs, " minlength=\"{min}\"");
            }
            if let Some(max) = number(&field.get_attr("max_length")?) {
                let _ = write!(attrs, " maxlength=\"{max}\"");
            }
            Ok(())
        };
        let range = |attrs: &mut String| -> Result<(), Error> {
            if let Some(min) = number(&field.get_attr("min")?) {
                let _ = write!(attrs, " min=\"{min}\"");
            }
            if let Some(max) = number(&field.get_attr("max")?) {
                let _ = write!(attrs, " max=\"{max}\"");
            }
            Ok(())
        };

        let mut html = format!("<div class=\"field{}\">\n", if errors.is_empty() { "" } else { " field-with-errors" });
        match input.as_str() {
            "checkbox" => {
                let checked = if value == "true" { " checked" } else { "" };
                let _ = writeln!(
                    html,
                    "  <label><input type=\"checkbox\" id=\"{id}\" name=\"{id}\" value=\"true\"{checked}> {}</label>",
                    esc(&label)
                );
            }
            "file" => {
                // The file it holds now (an edit, or a re-render after an error) rides along as a
                // signed token; choosing a new file replaces it.
                let current = field.get_attr("file")?;
                let has_file = !(current.is_none() || current.is_undefined());
                let attrs = if has_file { attrs.replacen(" required", "", 1) } else { attrs.clone() };
                let accept = spec_text("accept")?.map(|a| format!(" accept=\"{}\"", esc(&a))).unwrap_or_default();
                let _ = writeln!(html, "  <label for=\"{id}\">{}</label>", esc(&label));
                if has_file {
                    let url = text(&current, "url")?;
                    let filename = text(&current, "filename")?;
                    let _ = writeln!(html, "  <input type=\"hidden\" name=\"{id}\" value=\"{}\">", esc(&value));
                    if current.get_attr("is_image")?.is_true() {
                        let _ = writeln!(
                            html,
                            "  <p class=\"field-file\"><img src=\"{}\" alt=\"\" width=\"96\"> {}</p>",
                            esc(&url),
                            esc(&filename)
                        );
                    } else {
                        let _ = writeln!(
                            html,
                            "  <p class=\"field-file\"><a href=\"{}\">{}</a></p>",
                            esc(&url),
                            esc(&filename)
                        );
                    }
                }
                // With JavaScript the file goes straight to storage on submit (kiln/direct-upload).
                let _ = writeln!(
                    html,
                    "  <input type=\"file\" {attrs}{accept} data-direct-upload-url=\"/kiln/direct_uploads\">\n  <script type=\"module\">import \"kiln/direct-upload\"</script>"
                );
            }
            "textarea" => {
                lengths(&mut attrs)?;
                let _ = write!(
                    html,
                    "  <label for=\"{id}\">{}</label>\n  <textarea {attrs} rows=\"6\">{}</textarea>\n",
                    esc(&label),
                    esc(&value)
                );
            }
            "rich_text" => {
                // The editor (Lexxy) reads its starting HTML from `value` and submits under
                // `name`; Kiln's module configures it and the stylesheet skins it.
                let _ = write!(
                    html,
                    "  <label for=\"{id}\">{}</label>\n  <link rel=\"stylesheet\" href=\"{}\">\n  <lexxy-editor {attrs} class=\"kiln-rich-text lexxy-content\" data-direct-upload-url=\"/kiln/direct_uploads\" data-blob-url-template=\"/kiln/files/:signed_id/:filename\" value=\"{}\"></lexxy-editor>\n  <script type=\"module\">import \"kiln/rich-text\"</script>\n",
                    esc(&label),
                    crate::assets::rich_text_css_url(),
                    esc(&value)
                );
            }
            "select" => {
                let _ = write!(html, "  <label for=\"{id}\">{}</label>\n  <select {attrs}>\n", esc(&label));
                if !required || value.is_empty() {
                    html.push_str("    <option value=\"\">Choose…</option>\n");
                }
                for option in field.get_attr("options")?.try_iter()? {
                    let option = option.to_string();
                    let selected = if option == value { " selected" } else { "" };
                    let shown = option.replace('_', " ");
                    let shown = shown
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().chain(shown.chars().skip(1)).collect::<String>())
                        .unwrap_or_default();
                    let _ = writeln!(html, "    <option value=\"{}\"{selected}>{}</option>", esc(&option), esc(&shown));
                }
                html.push_str("  </select>\n");
            }
            other => {
                let kind = match other {
                    "decimal" => {
                        attrs.push_str(" step=\"any\"");
                        "number"
                    }
                    other => other,
                };
                if matches!(kind, "number") {
                    range(&mut attrs)?;
                } else {
                    lengths(&mut attrs)?;
                }
                let _ = write!(
                    html,
                    "  <label for=\"{id}\">{}</label>\n  <input type=\"{}\" {attrs} value=\"{}\">\n",
                    esc(&label),
                    esc(kind),
                    esc(&value)
                );
            }
        }
        if let Some(hint) = &o.hint {
            let _ = writeln!(html, "  <p class=\"field-hint\">{}</p>", esc(hint));
        }
        for (i, message) in errors.iter().enumerate() {
            let id_attr = if i == 0 { format!(" id=\"{id}-error\"") } else { String::new() };
            let _ = writeln!(html, "  <p class=\"field-error\"{id_attr}>{} {}</p>", esc(&label), esc(message));
        }
        html.push_str("</div>");
        Ok(html)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_text_links_its_assets_on_pages_but_not_in_mail() {
        let dir = std::env::temp_dir().join(format!("kiln-view-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("body.html.jinja"), "{{ body | rich_text }}").unwrap();
        let root: &'static str = Box::leak(dir.display().to_string().into_boxed_str());
        let context = BTreeMap::from([("body", "<p>Hi</p><pre>code</pre>")]);

        let page = render(root, "body.html.jinja", None, &context).unwrap();
        assert!(page.contains("<link rel=\"stylesheet\""), "{page}");
        assert!(page.contains("kiln/rich-text-display"), "{page}");

        let mail = render_mail(root, "body.html.jinja", None, &context).unwrap();
        assert_eq!(mail, "<div class=\"kiln-rich-text-content\"><p>Hi</p><pre>code</pre></div>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Named `repos::blob`, like an app's `src/actions/repos/blob.rs`.
    mod actions {
        pub mod repos {
            pub mod blob {
                use crate::Param;

                #[crate::action(GET "/{owner}/blob/{*path}")]
                pub async fn blob(owner: Param<String>, path: Param<String>) -> String {
                    format!("{} {}", *owner, *path)
                }
            }
        }
    }

    #[test]
    fn path_keeps_slashes_in_a_catch_all() {
        let dir = std::env::temp_dir().join(format!("kiln-view-path-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("link.html.jinja"), r#"{{ path("repos::blob", owner="a b", path=file) }}"#).unwrap();
        let root: &'static str = Box::leak(dir.display().to_string().into_boxed_str());
        let context = BTreeMap::from([("file", "src/my file.rs")]);

        let html = render(root, "link.html.jinja", None, &context).unwrap();
        assert_eq!(html, "/a%20b/blob/src/my%20file.rs");
        assert_eq!(actions::repos::blob::path("a b", "src/my file.rs"), html);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markdown_and_highlight_filters() {
        let dir = std::env::temp_dir().join(format!("kiln-view-md-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("md.html.jinja"),
            "{{ body | markdown }}|{{ code | highlight(\"a.rs\") }}|{% for l in code | highlight_lines(\"a.rs\") %}[{{ l }}]{% endfor %}",
        )
        .unwrap();
        let root: &'static str = Box::leak(dir.display().to_string().into_boxed_str());
        let context = BTreeMap::from([("body", "**hi** <script>x</script>"), ("code", "fn a() {}\nfn b() {}")]);

        let page = render(root, "md.html.jinja", None, &context).unwrap();
        let css = crate::markdown::stylesheet_url();
        assert!(page.starts_with(&format!("<link rel=\"stylesheet\" href=\"{css}\"><div class=\"kiln-markdown\"><p><strong>hi</strong> </p>\n</div>|")), "{page}");
        assert!(
            page.contains("<pre class=\"kiln-code\" data-language=\"Rust\"><code><span class=\"hl-source hl-rust\">"),
            "{page}"
        );
        assert_eq!(page.matches("[<span class=\"hl-source hl-rust\">").count(), 2, "{page}");

        let mail = render_mail(root, "md.html.jinja", None, &context).unwrap();
        assert!(mail.starts_with("<div class=\"kiln-markdown\">"), "{mail}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
