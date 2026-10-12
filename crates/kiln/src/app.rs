use std::{convert::Infallible, sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    BoxFuture, Db, Error, Record, Result,
    error_page::{self, ErrorReport, RequestInfo},
    routes, routes_table,
    routing::route_for,
    session::{self, COOKIE, Flash, REQUEST, RequestScope, SecretKey, Session, tokens_match},
    view::PendingRender,
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
    key: SecretKey,
    sign_ins: Vec<Arc<dyn SignIn>>,
}

/// A record type people sign in as, registered with [`App::signs_in`].
pub(crate) trait SignIn: Send + Sync {
    fn table(&self) -> &'static str;
    /// The template value, e.g. `current_user`.
    fn variable(&self) -> &str;
    fn sign_in_path(&self) -> &str;
    /// The signed-in record's template shape, or none.
    fn load<'a>(&'a self, session: &'a Session, db: &'a Db) -> BoxFuture<'a, Result<minijinja::Value>>;
}

struct Registration<T, S> {
    variable: String,
    sign_in_path: String,
    shape: fn(&T) -> S,
}

impl<T: Record, S: Serialize + Send + 'static> SignIn for Registration<T, S> {
    fn table(&self) -> &'static str {
        T::TABLE
    }

    fn variable(&self) -> &str {
        &self.variable
    }

    fn sign_in_path(&self) -> &str {
        &self.sign_in_path
    }

    fn load<'a>(&'a self, session: &'a Session, db: &'a Db) -> BoxFuture<'a, Result<minijinja::Value>> {
        Box::pin(async move {
            Ok(match session::signed_in::<T>(session, db).await? {
                Some(record) => minijinja::Value::from_serialize((self.shape)(&record)),
                None => minijinja::Value::from(()),
            })
        })
    }
}

/// `blog::records::user::User` → `user`.
fn record_variable<T>() -> String {
    let name = std::any::type_name::<T>();
    let name = name.rsplit("::").next().unwrap_or(name);
    let mut snake = String::new();
    for (i, c) in name.char_indices() {
        if c.is_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.extend(c.to_lowercase());
    }
    format!("current_{snake}")
}

struct Shared {
    key: SecretKey,
    sign_ins: Arc<[Arc<dyn SignIn>]>,
}

#[derive(Clone)]
struct RouteState {
    ctx: Ctx,
    sign_ins: Arc<[Arc<dyn SignIn>]>,
}

/// Largest form body kiln buffers for CSRF and method-override checks.
const FORM_LIMIT: usize = 2 * 1024 * 1024;

impl App {
    /// An app with a random session key, so sessions don't survive a restart.
    /// `kiln::Cli` sets a persistent key.
    pub fn new(db: Db) -> Self {
        Self { ctx: Ctx { db }, key: SecretKey::generate(), sign_ins: vec![] }
    }

    pub fn secret_key(mut self, key: SecretKey) -> Self {
        self.key = key;
        self
    }

    /// Registers a record type people sign in as.
    ///
    /// - Signed-out browsers asking for a page whose action takes `Current<T>` are sent to
    ///   `sign_in_path`.
    /// - Every template can read `current_<record>` (e.g. `current_user`): `shape(&record)`
    ///   for the signed-in `T`, or none. The shape is an allowlist, so templates never see
    ///   the whole record. It's loaded only for pages whose templates use it, and reuses
    ///   the record if the action already took `Current<T>`.
    ///
    /// ```ignore
    /// app.signs_in::<User, _>(sessions::new::path(), CurrentUser::from)
    ///    .signs_in::<Merchant, _>(merchant::sessions::new::path(), CurrentMerchant::from)
    /// ```
    pub fn signs_in<T: Record, S: Serialize + Send + 'static>(
        mut self,
        sign_in_path: impl Into<String>,
        shape: fn(&T) -> S,
    ) -> Self {
        self.sign_ins.push(Arc::new(Registration {
            variable: record_variable::<T>(),
            sign_in_path: sign_in_path.into(),
            shape,
        }));
        self
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    pub(crate) fn key(&self) -> &SecretKey {
        &self.key
    }

    /// An axum router with every `#[action]` linked into the binary, wrapped in kiln's
    /// request handling: sessions, flash, CSRF, method override, rendering, logging and
    /// error pages.
    pub fn router(&self) -> axum::Router {
        let sign_ins: Arc<[Arc<dyn SignIn>]> = self.sign_ins.clone().into();
        let route_state = RouteState { ctx: self.ctx.clone(), sign_ins: sign_ins.clone() };
        let routes = routes()
            .into_iter()
            .fold(axum::Router::<Ctx>::new(), |router, route| (route.mount)(router))
            .route_layer(middleware::from_fn_with_state(route_state, route_checks))
            .with_state(self.ctx.clone());
        let shared = Arc::new(Shared { key: self.key.clone(), sign_ins });
        // Method override must happen before routing, so request handling wraps the whole
        // router instead of each route.
        axum::Router::new().fallback_service(routes).layer(middleware::from_fn_with_state(shared, handle_request))
    }

    pub async fn serve(self, addr: &str) -> Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("{}\n\nkiln listening on http://{addr}", routes_table());
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
}

/// The action that handled a request, recorded for logs and error pages.
#[derive(Clone, Copy)]
struct ActionName(&'static str);

/// The pipeline of the route that handled a request.
#[derive(Clone, Copy)]
struct PipelineName(&'static str);

/// The CSRF token a request submitted, read before routing from the header or form body.
#[derive(Clone)]
struct SubmittedCsrf(Option<String>);

/// Runs once a request matched a route: checks CSRF on browser writes, runs the action
/// inside the request scope, then renders its view. Unmatched requests get their 404/405.
async fn route_checks(State(state): State<RouteState>, mut request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|matched| route_for(request.method().as_str(), matched.as_str()));
    // A path without this method: let the router answer 405.
    let Some(route) = route else { return next.run(request).await };

    let scope = if route.pipeline == "api" {
        // API routes ignore the cookie session. CSRF protection relies on it, so an API
        // route authenticated by cookie would be forgeable from other sites.
        request.extensions_mut().insert(Session::default());
        Some(RequestScope::default())
    } else {
        let session = request.extensions().get::<Session>().cloned().unwrap_or_default();
        let mutating = !matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
        let csrf_ok = !mutating
            || {
                let sent = request.extensions().get::<SubmittedCsrf>().and_then(|s| s.0.as_deref());
                matches!((sent, session.existing_csrf_token()), (Some(sent), Some(expected)) if tokens_match(sent, &expected))
            };
        csrf_ok.then(|| {
            let flash = session.take_flash();
            RequestScope { session, flash, cache: Default::default() }
        })
    };

    let mut response = match scope {
        None => Error::Csrf.into_response(),
        Some(scope) => {
            REQUEST
                .scope(scope.clone(), async move {
                    let response = next.run(request).await;
                    render_page(response, &state, &scope).await
                })
                .await
        }
    };
    response.extensions_mut().insert(ActionName(route.name()));
    response.extensions_mut().insert(PipelineName(route.pipeline));
    response
}

/// Views are rendered here, after the action, so the `current_*` values their templates
/// use can be loaded first (and reused if the action already loaded them).
async fn render_page(mut response: Response, state: &RouteState, scope: &RequestScope) -> Response {
    let Some(page) = response.extensions_mut().remove::<PendingRender>() else { return response };
    let used = match page.variables() {
        Ok(used) => used,
        Err(e) => return e.into_response(),
    };
    for sign_in in state.sign_ins.iter() {
        let value = if used.contains(sign_in.variable()) {
            match sign_in.load(&scope.session, &state.ctx.db).await {
                Ok(value) => value,
                Err(e) => return e.into_response(),
            }
        } else {
            minijinja::Value::from(())
        };
        scope.cache.templates.lock().unwrap_or_else(|e| e.into_inner()).insert(sign_in.variable().to_owned(), value);
    }
    match page.render() {
        Ok(html) => {
            let (mut parts, _) = response.into_parts();
            parts.headers.remove(header::CONTENT_LENGTH);
            parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
            Response::from_parts(parts, Body::from(html))
        }
        Err(e) => e.into_response(),
    }
}

fn cookie<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
}

/// Logs one line per request under the `kiln::request` target and runs the browser
/// pipeline: session, flash, CSRF, `_method` override, and error pages (HTML for
/// browsers, Markdown otherwise).
async fn handle_request(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let full_path = parts.uri.path_and_query().map_or_else(|| path.clone(), ToString::to_string);
    let wants_html = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));

    let session = cookie(&parts, COOKIE).and_then(|c| shared.key.verify(c)).map(Session::from_data).unwrap_or_default();

    // Form bodies are buffered so `_method` and `_csrf` can be read before routing.
    let mutating = !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
    let is_form = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
    let mut fields: Vec<(String, String)> = vec![];
    let body = if mutating && is_form {
        match axum::body::to_bytes(body, FORM_LIMIT).await {
            Ok(bytes) => {
                fields = serde_urlencoded::from_bytes(&bytes).unwrap_or_default();
                Body::from(bytes)
            }
            Err(_) => Body::empty(),
        }
    } else {
        body
    };
    let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    if parts.method == Method::POST
        && let Some(method) = field("_method").map(str::to_ascii_uppercase)
        && matches!(method.as_str(), "PUT" | "PATCH" | "DELETE")
    {
        parts.method = method.parse().expect("valid method");
    }
    let method = parts.method.clone();

    let sent = parts.headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).or_else(|| field("_csrf"));
    parts.extensions.insert(SubmittedCsrf(sent.map(str::to_owned)));
    parts.extensions.insert(session.clone());
    let mut response = next.run(Request::from_parts(parts, body)).await;

    let api = response.extensions().get::<PipelineName>().is_some_and(|p| p.0 == "api");
    if let Some(flash) = response.extensions_mut().remove::<Flash>()
        && !api
    {
        session.set_flash(&flash);
    }
    let action = response.extensions().get::<ActionName>().map(|a| a.0);
    let status = response.status().as_u16();
    let elapsed = started.elapsed();
    let report = response
        .extensions_mut()
        .remove::<ErrorReport>()
        .or_else(|| ErrorReport::for_routing(response.status(), method.as_str(), &path));

    response = match report {
        None => {
            tracing::info!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed);
            response
        }
        Some(report) => {
            let (code, message) = (report.code, report.message.as_str());
            if report.status.is_server_error() {
                tracing::error!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed, code, message);
            } else {
                tracing::info!(target: "kiln::request", method = %method, path, action = action.unwrap_or("-"), status, ?elapsed, code, message);
            }
            // Browsers asking for a page that needs sign-in go to that record type's sign-in page.
            let sign_in = report
                .sign_in_for
                .filter(|_| wants_html && method == Method::GET && !api)
                .and_then(|table| shared.sign_ins.iter().find(|s| s.table() == table));
            match sign_in {
                Some(sign_in) => {
                    session.set_flash(&Flash([("alert".to_owned(), "Sign in to continue.".to_owned())].into()));
                    (StatusCode::SEE_OTHER, [(header::LOCATION, sign_in.sign_in_path().to_owned())]).into_response()
                }
                None => {
                    let request = RequestInfo { method: method.as_str(), path: &full_path, action };
                    let format = match (api, wants_html) {
                        (true, _) => error_page::Format::Json,
                        (false, true) => error_page::Format::Html,
                        (false, false) => error_page::Format::Markdown,
                    };
                    error_page::render(response, &report, &request, format)
                }
            }
        }
    };

    if session.changed() && !api {
        let data = session.data();
        let secure = if cfg!(debug_assertions) { "" } else { "; Secure" };
        let cookie = if data.is_empty() {
            format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}")
        } else {
            format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Lax{secure}", shared.key.sign(&data))
        };
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}
