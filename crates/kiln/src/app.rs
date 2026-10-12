use std::{convert::Infallible, time::Instant};

use axum::{
    extract::{FromRequestParts, MatchedPath, Request},
    http::{header, request::Parts},
    middleware::{self, Next},
    response::Response,
};

use crate::{
    Db, Result, RouteDef,
    error_page::{self, ErrorReport, RequestInfo},
    routes, routes_table,
    routing::route_for,
};

/// Request context: the app's dependencies, built once at boot. Extract it in any action.
#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
}

impl FromRequestParts<Ctx> for Ctx {
    type Rejection = Infallible;

    async fn from_request_parts(_parts: &mut Parts, state: &Ctx) -> Result<Self, Self::Rejection> {
        Ok(state.clone())
    }
}

pub struct App {
    ctx: Ctx,
}

impl App {
    pub fn new(db: Db) -> Self {
        Self { ctx: Ctx { db } }
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    /// An axum router with every `#[action]` linked into the binary, plus request logging
    /// and error pages.
    pub fn router(&self) -> axum::Router {
        routes()
            .into_iter()
            .fold(axum::Router::<Ctx>::new(), |router, route| (route.mount)(router))
            .layer(middleware::from_fn(handle_request))
            .with_state(self.ctx.clone())
    }

    pub async fn serve(self, addr: &str) -> Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("{}\n\nkiln listening on http://{addr}", routes_table());
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

/// Logs one line per request under the `kiln::request` target, e.g.
/// `method=GET path=/posts/hello-kiln action=posts::show status=200 elapsed=1.2ms`,
/// and turns error responses into error pages (HTML for browsers, Markdown otherwise).
async fn handle_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let full_path = request.uri().path_and_query().map_or_else(|| path.clone(), ToString::to_string);
    let action = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|matched| route_for(method.as_str(), matched.as_str()))
        .map(RouteDef::name);
    let wants_html = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    let started = Instant::now();
    let mut response = next.run(request).await;
    let status = response.status().as_u16();
    let elapsed = started.elapsed();

    let report = response
        .extensions_mut()
        .remove::<ErrorReport>()
        .or_else(|| ErrorReport::for_routing(response.status(), method.as_str(), &path));
    let Some(report) = report else {
        tracing::info!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed);
        return response;
    };

    let (code, message) = (report.code, report.message.as_str());
    if report.status.is_server_error() {
        tracing::error!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed, code, message);
    } else {
        tracing::info!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed, code, message);
    }
    let request = RequestInfo { method: method.as_str(), path: &full_path, action };
    error_page::render(response, &report, &request, wants_html)
}
