use std::{collections::HashMap, fmt::Display, ops::Deref, str::FromStr, sync::OnceLock};

use axum::{
    extract::{FromRequestParts, Path},
    http::request::Parts,
    response::{IntoResponse, Response},
};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::{Ctx, Error};

/// A route registered by `#[action]`. Collected at link time, so adding an endpoint never
/// means editing a shared routes file.
pub struct RouteDef {
    pub method: &'static str,
    pub path: &'static str,
    pub module: &'static str,
    pub function: &'static str,
    pub file: &'static str,
    pub line: u32,
    /// `browser` (session, CSRF, HTML errors) or `api` (no session, JSON errors).
    pub pipeline: &'static str,
    /// The action's arguments as written, `(name, type)`: `("user", "Current<User>")`.
    pub args: &'static [(&'static str, &'static str)],
    /// The return type as written, e.g. `Result<Redirect>`.
    pub returns: &'static str,
    /// `scope = "repo:write"`: the scope an API token needs here (`"any"` for any token).
    /// `None` on API routes means they sign nobody in; such routes refuse tokens.
    pub scope: Option<&'static str>,
    pub mount: fn(axum::Router<Ctx>) -> axum::Router<Ctx>,
}

inventory::collect!(RouteDef);

/// The route's `scope`, put on each request by its `#[action]` so `Current<T>` can check an
/// API token against it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct RequiredScope(pub Option<&'static str>);

impl RouteDef {
    /// The route's name, derived from its module: `app::actions::posts::show` → `posts::show`.
    pub fn name(&self) -> &'static str {
        match self.module.find("::actions::") {
            Some(i) => &self.module[i + "::actions::".len()..],
            None => self.module,
        }
    }

    /// The capture names in order; a catch-all `{*path}` yields `path`.
    pub fn captures(&self) -> impl Iterator<Item = &'static str> {
        self.path.split('/').filter_map(capture_name)
    }
}

/// `{slug}` → `slug`, `{*path}` → `path`, anything else → `None`.
pub(crate) fn capture_name(segment: &str) -> Option<&str> {
    let inner = segment.strip_prefix('{')?.strip_suffix('}')?;
    Some(inner.strip_prefix('*').unwrap_or(inner))
}

/// Every registered route, sorted by path then method.
pub fn routes() -> Vec<&'static RouteDef> {
    let mut routes: Vec<&'static RouteDef> = inventory::iter::<RouteDef>.into_iter().collect();
    routes.sort_by_key(|r| (r.path, r.method));
    routes
}

/// The route table as aligned text, like `kiln routes`.
pub fn routes_table() -> String {
    let routes = routes();
    let path_width = routes.iter().map(|r| r.path.len()).max().unwrap_or(0);
    let name_width = routes.iter().map(|r| r.name().len()).max().unwrap_or(0);
    routes
        .iter()
        .map(|r| format!("{:<6} {:<path_width$}  {:<name_width$}  {}:{}", r.method, r.path, r.name(), r.file, r.line))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn route_named(name: &str) -> Option<&'static RouteDef> {
    inventory::iter::<RouteDef>.into_iter().find(|r| r.name() == name)
}

/// The route registered for a method and path pattern. Built once, then a hash lookup.
pub(crate) fn route_for(method: &str, path: &str) -> Option<&'static RouteDef> {
    static INDEX: OnceLock<HashMap<(&'static str, &'static str), &'static RouteDef>> = OnceLock::new();
    INDEX.get_or_init(|| routes().into_iter().map(|r| ((r.method, r.path), r)).collect()).get(&(method, path)).copied()
}

/// A typed path segment. The `#[action]` macro matches `slug: Param<String>` to the
/// `{slug}` capture in the route, and fails to compile if either side is missing.
///
/// A catch-all `{*path}` as the last segment takes the rest of the URL, slashes included:
/// `/{owner}/{repo}/tree/{*path}` gives `path == "src/lib/mod.rs"`. Each segment is
/// percent-decoded, so `%2F` arrives as `/` too, and `..` is passed through as written:
/// treat the value as untrusted before touching the filesystem with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param<T>(pub T);

impl<T> Param<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Param<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// An argument accepted by a generated path helper for a capture of type `T`.
pub trait PathArg<T> {
    /// The value as one percent-encoded segment: `/` becomes `%2F`.
    fn segment(&self) -> String;
    /// The value for a catch-all `{*path}`: each `/`-separated piece encoded, slashes kept.
    fn segments(&self) -> String;
}

impl<T: Display> PathArg<T> for T {
    fn segment(&self) -> String {
        encode_segment(&self.to_string())
    }

    fn segments(&self) -> String {
        encode_segments(&self.to_string())
    }
}

impl<T: Display> PathArg<T> for &T {
    fn segment(&self) -> String {
        encode_segment(&self.to_string())
    }

    fn segments(&self) -> String {
        encode_segments(&self.to_string())
    }
}

impl PathArg<String> for &str {
    fn segment(&self) -> String {
        encode_segment(self)
    }

    fn segments(&self) -> String {
        encode_segments(self)
    }
}

const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

pub(crate) fn encode_segment(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

pub(crate) fn encode_segments(s: &str) -> String {
    s.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// What an action argument before the last must be: something read from the request's
/// head. `#[action]` checks each argument against it, so a wrong one is named instead of
/// failing as an opaque `Handler` error.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't be an action argument",
    label = "not something an action can take",
    note = "action arguments are extractors: `Ctx`, `Current<User>`, `Param<T>` for a route capture, `QueryParams<T>`, `HeaderMap`, `Session`, and last, `Form<T>` or `Json<T>` for the body",
    note = "to work with a record, take its id (`id: Param<Id<Post>>`) and load it in the action: `Post::find(*id).one_or_404(&ctx.db).await?`"
)]
pub trait ActionArg {}

impl<T: axum::extract::FromRequestParts<Ctx>> ActionArg for T {}

/// The last action argument, which may also read the body.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't be an action argument",
    label = "not something an action can take",
    note = "action arguments are extractors: `Ctx`, `Current<User>`, `Param<T>` for a route capture, `QueryParams<T>`, `HeaderMap`, `Session`, and last, `Form<T>` or `Json<T>` for the body",
    note = "to work with a record, take its id (`id: Param<Id<Post>>`) and load it in the action: `Post::find(*id).one_or_404(&ctx.db).await?`"
)]
pub trait LastActionArg<M> {}

impl<M, T: axum::extract::FromRequest<Ctx, M>> LastActionArg<M> for T {}

#[doc(hidden)]
pub fn action_arg<T: ActionArg>() {}

#[doc(hidden)]
pub fn last_action_arg<T: LastActionArg<M>, M>() {}

/// Raw path parameters, parsed per capture by `#[action]`-generated handlers.
#[doc(hidden)]
pub struct RawParams(HashMap<String, String>);

impl RawParams {
    pub fn parse<T>(&self, name: &str) -> Result<T, Error>
    where
        T: FromStr,
        T::Err: Display,
    {
        let raw = self.0.get(name).ok_or_else(|| Error::BadRequest(format!("missing path parameter `{name}`")))?;
        raw.parse().map_err(|e| Error::BadRequest(format!("invalid path parameter `{name}`: {e}")))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RawParams {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map(|Path(params)| RawParams(params))
            .map_err(IntoResponse::into_response)
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn catch_all_values_keep_their_slashes() {
        assert_eq!(PathArg::<String>::segments(&"src/my file.rs"), "src/my%20file.rs");
        assert_eq!(PathArg::<String>::segment(&"src/my file.rs"), "src%2Fmy%20file.rs");
        assert_eq!(encode_segments("a?b/c#d/"), "a%3Fb/c%23d/");
        assert_eq!(capture_name("{*path}"), Some("path"));
        assert_eq!(capture_name("{slug}"), Some("slug"));
        assert_eq!(capture_name("tree"), None);
    }

    fn router(paths: &[&'static str]) -> Router {
        paths.iter().copied().fold(Router::new(), |router, path| router.route(path, get(move || async move { path })))
    }

    async fn matched(app: &Router, uri: &str) -> String {
        let response = app.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
        String::from_utf8(axum::body::to_bytes(response.into_body(), 1024).await.unwrap().to_vec()).unwrap()
    }

    /// `kiln check` (K0110) relies on this: literals beside a catch-all are resolved by
    /// priority, but a capture and a catch-all in the same place can't both be registered.
    #[tokio::test]
    async fn axum_prefers_literals_and_rejects_captures_beside_catch_alls() {
        let app = router(&["/{o}/{r}/tree/{*path}", "/{o}/{r}/tree/README", "/{o}/{r}/{*rest}"]);
        assert_eq!(matched(&app, "/a/b/tree/README").await, "/{o}/{r}/tree/README");
        assert_eq!(matched(&app, "/a/b/tree/README/x").await, "/{o}/{r}/tree/{*path}");
        assert_eq!(matched(&app, "/a/b/issues/1").await, "/{o}/{r}/{*rest}");

        for conflict in [["/a/{x}", "/a/{*p}"], ["/a/{x}/b", "/a/{*p}"], ["/a/{*p}", "/a/{*q}"]] {
            assert!(std::panic::catch_unwind(|| router(&conflict)).is_err(), "{conflict:?} should conflict");
        }
    }
}
