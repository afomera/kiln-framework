//! Typed query strings: `?q=parser&sort=stars&tag=rust&tag=web` read into a
//! `#[derive(Form)]` struct, with links that change one parameter and keep the rest.
//!
//! ```ignore
//! #[derive(Form, Default)]
//! pub struct Filters {
//!     pub q: Option<String>,
//!     pub sort: Sort,          // a `DbEnum` with a `#[default]` variant
//!     pub tag: Vec<String>,    // `?tag=a&tag=b`
//! }
//!
//! #[action(GET "/repos")]
//! pub async fn index(ctx: Ctx, filters: QueryParams<Filters>) -> Result<Index> {
//!     let repos = Repo::search(&filters).all(&ctx.db).await?;
//!     Ok(Index { repos, filters })   // the template: {{ filters.with("sort", "stars") }}
//! }
//! ```

use std::{fmt::Display, ops::Deref, sync::Arc};

use axum::{extract::FromRequestParts, http::request::Parts};
use minijinja::{
    Error as TemplateError, ErrorKind, State,
    value::{Enumerator, Object, Value, ValueKind},
};
use serde::{Serialize, Serializer};

use crate::form::{Errors, Fields, FormInput};

/// A query string read into `T`, a `#[derive(Form, Default)]` struct. Take it as an action
/// argument, in any position.
///
/// - Field types, `#[validate]` rules and enums work as in a form. Keys `T` doesn't declare
///   are ignored (query strings carry extras: `utm_source`, a cache buster), and a key
///   given several times fills a `Vec<T>` field.
/// - A missing key takes its value from `T::default()`.
/// - **A value that doesn't parse or breaks a rule also falls back to its default**, so a
///   stale or hand-edited link still shows the page instead of an error; the messages are
///   in [`QueryParams::errors`] for actions that want to refuse instead
///   (`return Err(Error::Invalid(filters.errors().clone()))`).
///
/// Links: [`with`](Self::with), [`without`](Self::without) and [`toggle`](Self::toggle)
/// return this page's path with one parameter changed and the others kept; parameters at
/// their default are left out. Templates call the same methods on the view field:
/// `{{ filters.with("sort", "stars") }}`, `{{ filters.sort }}` (a value),
/// `filters.values("tag")`, `filters.has("tag", "rust")`, `filters.href()`,
/// `filters.query_string()`.
pub struct QueryParams<T> {
    value: T,
    links: Arc<Links>,
    errors: Errors,
}

impl<T: FormInput + Default> QueryParams<T> {
    /// Reads `query` (without the `?`) as the query string of `path`.
    pub fn parse(path: impl Into<String>, query: Option<&str>) -> Self {
        let pairs: Vec<(String, String)> = query.and_then(|q| serde_urlencoded::from_str(q).ok()).unwrap_or_default();
        let defaults = T::default().to_fields();
        let mut fields = defaults.clone();
        for name in T::FIELDS {
            let given: Vec<String> = pairs.iter().filter(|(k, _)| k == name).map(|(_, v)| v.clone()).collect();
            if !given.is_empty() {
                fields.set_all(*name, given);
            }
        }
        let (value, errors) = match T::from_fields(&fields) {
            Ok(value) => (value, Errors::default()),
            Err(errors) => {
                for (name, _) in errors.iter() {
                    fields.set_all(name, defaults.get_all(name).to_vec());
                }
                (T::from_fields(&fields).unwrap_or_default(), errors)
            }
        };
        Self::build(path.into(), value, errors)
    }

    /// `value` as the query string of `path`, for links to a page with these parameters:
    /// `QueryParams::of(repos::index::path(), Filters { sort: Sort::Stars, ..Default::default() }).href()`.
    pub fn of(path: impl Into<String>, value: T) -> Self {
        Self::build(path.into(), value, Errors::default())
    }

    fn build(path: String, value: T, errors: Errors) -> Self {
        let pairs = |fields: &Fields| T::FIELDS.iter().map(|name| (*name, fields.get_all(name).to_vec())).collect();
        let links = Links { path, current: pairs(&value.to_fields()), defaults: pairs(&T::default().to_fields()) };
        Self { value, links: Arc::new(links), errors }
    }
}

impl<T> QueryParams<T> {
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Why values were replaced by their defaults; empty when everything parsed.
    pub fn errors(&self) -> &Errors {
        &self.errors
    }

    /// This page with these parameters: `/repos?sort=stars`.
    pub fn href(&self) -> String {
        self.links.href(&self.links.current)
    }

    /// The parameters that differ from the defaults, `sort=stars&tag=rust`, without the `?`
    /// (empty when there are none).
    pub fn to_query_string(&self) -> String {
        self.links.query_string(&self.links.current)
    }

    /// This page with `key` set to `value` and every other parameter kept.
    pub fn with(&self, key: &str, value: impl Display) -> String {
        self.links.with(key, vec![value.to_string()])
    }

    /// This page with `key` back at its default.
    pub fn without(&self, key: &str) -> String {
        self.links.without(key)
    }

    /// For `Vec` fields: this page with `value` added to `key`'s values, or removed if it's
    /// already there.
    pub fn toggle(&self, key: &str, value: impl Display) -> String {
        self.links.toggle(key, &value.to_string())
    }
}

impl<T> Deref for QueryParams<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for QueryParams<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryParams").field("value", &self.value).field("href", &self.href()).finish()
    }
}

impl<T: Clone> Clone for QueryParams<T> {
    fn clone(&self) -> Self {
        Self { value: self.value.clone(), links: self.links.clone(), errors: self.errors.clone() }
    }
}

impl<T: FormInput + Default, S: Send + Sync> FromRequestParts<S> for QueryParams<T> {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::parse(parts.uri.path(), parts.uri.query()))
    }
}

/// In templates it's an object with the link methods; elsewhere (JSON) a map of the values.
impl<T> Serialize for QueryParams<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Value::from_dyn_object(self.links.clone()).serialize(serializer)
    }
}

/// The parameters as strings, for building links. Not generic, so templates can hold it.
#[derive(Debug)]
struct Links {
    path: String,
    /// Each field's values, in declaration order.
    current: Vec<(&'static str, Vec<String>)>,
    defaults: Vec<(&'static str, Vec<String>)>,
}

impl Links {
    fn query_string(&self, fields: &[(&str, Vec<String>)]) -> String {
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        for (name, values) in fields {
            let default = self.defaults.iter().find(|(n, _)| n == name).map(|(_, v)| v);
            if Some(values) == default {
                continue;
            }
            pairs.extend(values.iter().map(|v| (*name, v.as_str())));
        }
        serde_urlencoded::to_string(&pairs).unwrap_or_default()
    }

    fn href(&self, fields: &[(&str, Vec<String>)]) -> String {
        match self.query_string(fields) {
            query if query.is_empty() => self.path.clone(),
            query => format!("{}?{query}", self.path),
        }
    }

    /// The current values with `key` replaced. Keys the struct doesn't declare are added
    /// at the end, so a typo shows up in the link instead of vanishing.
    fn changed<'a>(&'a self, key: &'a str, values: Vec<String>) -> Vec<(&'a str, Vec<String>)> {
        let mut fields: Vec<(&str, Vec<String>)> = self.current.clone();
        match fields.iter_mut().find(|(n, _)| *n == key) {
            Some((_, slot)) => *slot = values,
            None => fields.push((key, values)),
        }
        fields
    }

    fn with(&self, key: &str, values: Vec<String>) -> String {
        self.href(&self.changed(key, values))
    }

    fn without(&self, key: &str) -> String {
        let default = self.defaults.iter().find(|(n, _)| *n == key).map(|(_, v)| v.clone()).unwrap_or_default();
        self.with(key, default)
    }

    fn toggle(&self, key: &str, value: &str) -> String {
        let mut values = self.values(key).to_vec();
        match values.iter().position(|v| v == value) {
            Some(at) => {
                values.remove(at);
            }
            None => values.push(value.to_owned()),
        }
        self.with(key, values)
    }

    fn values(&self, key: &str) -> &[String] {
        self.current.iter().find(|(n, _)| *n == key).map_or(&[], |(_, v)| v.as_slice())
    }
}

impl Object for Links {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let key = key.as_str()?;
        let (_, values) = self.current.iter().find(|(n, _)| *n == key)?;
        Some(Value::from(values.last().cloned().unwrap_or_default()))
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(self.current.iter().map(|(name, _)| Value::from(*name)).collect())
    }

    fn call_method(self: &Arc<Self>, _: &State<'_, '_>, method: &str, args: &[Value]) -> Result<Value, TemplateError> {
        let text = |at: usize| -> Result<String, TemplateError> {
            let arg = args.get(at).ok_or_else(|| {
                TemplateError::new(ErrorKind::MissingArgument, format!("`{method}` needs a parameter name and value"))
            })?;
            // As `to_field` writes them: `true` (not `True`), and nothing for none.
            Ok(match arg.kind() {
                ValueKind::Bool if arg.is_true() => "true".to_owned(),
                ValueKind::Bool => "false".to_owned(),
                ValueKind::None | ValueKind::Undefined => String::new(),
                _ => arg.as_str().map_or_else(|| arg.to_string(), str::to_owned),
            })
        };
        let arity = |n: usize| {
            if args.len() == n {
                Ok(())
            } else {
                Err(TemplateError::new(
                    ErrorKind::InvalidOperation,
                    format!("`{method}` takes {n} argument{}", if n == 1 { "" } else { "s" }),
                ))
            }
        };
        match method {
            "with" => {
                arity(2)?;
                Ok(Value::from(self.with(&text(0)?, vec![text(1)?])))
            }
            "without" => {
                arity(1)?;
                Ok(Value::from(self.without(&text(0)?)))
            }
            "toggle" => {
                arity(2)?;
                Ok(Value::from(self.toggle(&text(0)?, &text(1)?)))
            }
            "values" => {
                arity(1)?;
                Ok(Value::from(self.values(&text(0)?).to_vec()))
            }
            "has" => {
                arity(2)?;
                let value = text(1)?;
                Ok(Value::from(self.values(&text(0)?).contains(&value)))
            }
            "href" => {
                arity(0)?;
                Ok(Value::from(self.href(&self.current)))
            }
            "query_string" => {
                arity(0)?;
                Ok(Value::from(self.query_string(&self.current)))
            }
            _ => Err(TemplateError::new(
                ErrorKind::UnknownMethod,
                format!(
                    "query parameters have no method `{method}`; they have with, without, toggle, values, has, href and query_string"
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::{FieldSpec, FormField};

    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    enum Sort {
        #[default]
        Newest,
        Stars,
    }

    impl FormField for Sort {
        const INPUT: &'static str = "select";
        const OPTIONS: &'static [&'static str] = &["newest", "stars"];

        fn from_field(raw: Option<&str>) -> Result<Self, String> {
            match raw.map(str::trim) {
                Some("newest") => Ok(Sort::Newest),
                Some("stars") => Ok(Sort::Stars),
                _ => Err("must be one of: newest, stars".into()),
            }
        }

        fn to_field(&self) -> String {
            match self {
                Sort::Newest => "newest".into(),
                Sort::Stars => "stars".into(),
            }
        }
    }

    /// What `#[derive(Form)]` writes, by hand (the derive's paths are `::kiln::..`).
    #[derive(Debug, Clone, PartialEq)]
    struct Filters {
        q: Option<String>,
        sort: Sort,
        page: i64,
        tag: Vec<String>,
    }

    impl Default for Filters {
        fn default() -> Self {
            Self { q: None, sort: Sort::Newest, page: 1, tag: vec![] }
        }
    }

    impl FormInput for Filters {
        const FIELDS: &'static [&'static str] = &["q", "sort", "page", "tag"];
        const SPECS: &'static [FieldSpec] = &[];

        fn from_fields(fields: &Fields) -> Result<Self, Errors> {
            let mut errors = Errors::default();
            let q = fields.parse("q", &mut errors);
            let sort = fields.parse("sort", &mut errors);
            let page = fields.parse::<i64>("page", &mut errors);
            if let Some(page) = page {
                crate::form::rules::range(&page, Some(1.0), None, "page", &mut errors);
            }
            let tag = fields.parse("tag", &mut errors);
            if !errors.is_empty() {
                return Err(errors);
            }
            Ok(Self { q: q.unwrap(), sort: sort.unwrap(), page: page.unwrap(), tag: tag.unwrap() })
        }

        fn to_fields(&self) -> Fields {
            let mut fields = Fields::default();
            fields.set_all("q", self.q.to_values());
            fields.set_all("sort", self.sort.to_values());
            fields.set_all("page", self.page.to_values());
            fields.set_all("tag", self.tag.to_values());
            fields
        }
    }

    fn parse(query: &str) -> QueryParams<Filters> {
        QueryParams::parse("/repos", Some(query))
    }

    #[test]
    fn reads_values_defaults_and_repeated_keys() {
        let empty = QueryParams::<Filters>::parse("/repos", None);
        assert_eq!(*empty, Filters::default());
        assert_eq!(empty.href(), "/repos");

        let filters = parse("q=parser+bug&sort=stars&tag=rust&tag=web&utm_source=mail&page=2");
        assert_eq!(filters.q.as_deref(), Some("parser bug"));
        assert_eq!(filters.sort, Sort::Stars);
        assert_eq!(filters.page, 2);
        assert_eq!(filters.tag, ["rust", "web"]);
        assert!(filters.errors().is_empty());
        // Unknown keys are dropped from links; defaults are left out.
        assert_eq!(filters.to_query_string(), "q=parser+bug&sort=stars&page=2&tag=rust&tag=web");
    }

    #[test]
    fn bad_values_fall_back_to_their_defaults() {
        let filters = parse("sort=oldest&page=0&q=kept");
        assert_eq!(filters.sort, Sort::Newest);
        assert_eq!(filters.page, 1, "breaking `range(min = 1)` falls back too");
        assert_eq!(filters.q.as_deref(), Some("kept"));
        assert_eq!(filters.errors().get("sort"), ["must be one of: newest, stars"]);
        assert_eq!(filters.errors().get("page"), ["must be at least 1"]);
        assert_eq!(parse("page=abc").page, 1);
        assert_eq!(parse("page=").page, 1);
    }

    #[test]
    fn links_change_one_parameter_and_keep_the_rest() {
        let filters = parse("q=a%26b&tag=rust&page=3");
        assert_eq!(filters.with("sort", "stars"), "/repos?q=a%26b&sort=stars&page=3&tag=rust");
        assert_eq!(filters.with("page", 1), "/repos?q=a%26b&tag=rust", "a default value drops out");
        assert_eq!(filters.without("q"), "/repos?page=3&tag=rust");
        assert_eq!(filters.toggle("tag", "web"), "/repos?q=a%26b&page=3&tag=rust&tag=web");
        assert_eq!(filters.toggle("tag", "rust"), "/repos?q=a%26b&page=3");
        assert_eq!(
            QueryParams::of("/repos", Filters { sort: Sort::Stars, ..Filters::default() }).href(),
            "/repos?sort=stars"
        );
    }

    #[test]
    fn templates_call_the_same_methods() {
        let filters = parse("q=x&tag=rust");
        let env = minijinja::Environment::new();
        let render = |source: &str| {
            env.render_str(source, minijinja::context! { filters => Value::from_serialize(&filters) }).unwrap()
        };
        assert_eq!(render(r#"{{ filters.with("sort", "stars") }}"#), "/repos?q=x&sort=stars&tag=rust");
        assert_eq!(render(r#"{{ filters.q }}|{{ filters.sort }}|{{ filters.page }}"#), "x|newest|1");
        let checks = r#"{% if filters.has("tag", "rust") and not filters.has("tag", "web") %}on{% endif %}"#;
        assert_eq!(render(&format!(r#"{checks} {{{{ filters.values("tag") | join(",") }}}}"#)), "on rust");
        assert_eq!(render(r#"{{ filters.toggle("tag", "rust") }}"#), "/repos?q=x");
        assert_eq!(render(r#"{{ filters.href() }} {{ filters.query_string() }}"#), "/repos?q=x&tag=rust q=x&tag=rust");
        let err = env
            .render_str("{{ filters.sorted() }}", minijinja::context! { filters => Value::from_serialize(&filters) });
        assert!(err.unwrap_err().to_string().contains("no method `sorted`"));
    }
}
