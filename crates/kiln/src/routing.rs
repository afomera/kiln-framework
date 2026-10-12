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
    pub mount: fn(axum::Router<Ctx>) -> axum::Router<Ctx>,
}

inventory::collect!(RouteDef);

impl RouteDef {
    /// The route's name, derived from its module: `app::actions::posts::show` → `posts::show`.
    pub fn name(&self) -> &'static str {
        match self.module.find("::actions::") {
            Some(i) => &self.module[i + "::actions::".len()..],
            None => self.module,
        }
    }

    pub fn captures(&self) -> impl Iterator<Item = &'static str> {
        self.path.split('/').filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
    }
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
    fn segment(&self) -> String;
}

impl<T: Display> PathArg<T> for T {
    fn segment(&self) -> String {
        encode_segment(&self.to_string())
    }
}

impl<T: Display> PathArg<T> for &T {
    fn segment(&self) -> String {
        encode_segment(&self.to_string())
    }
}

impl PathArg<String> for &str {
    fn segment(&self) -> String {
        encode_segment(self)
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
