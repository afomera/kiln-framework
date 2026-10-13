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
    assets::{self, Assets},
    error_page::{self, ErrorReport, RequestInfo},
    realtime, routes, routes_table,
    routing::route_for,
    session::{self, COOKIE, Flash, REQUEST, RequestScope, SecretKey, Session, tokens_match},
    turbo::{self, PendingStream, Turbo},
    view::PendingRender,
};

/// Request context: the app's dependencies, built once at boot. Extract it in any action.
#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    key: SecretKey,
    provided: Arc<std::collections::HashMap<std::any::TypeId, Arc<dyn std::any::Any + Send + Sync>>>,
}

impl Ctx {
    pub(crate) fn signing_key(&self) -> &SecretKey {
        &self.key
    }

    /// Signed, expiring tokens for links in email (password resets, confirmations).
    pub fn tokens(&self) -> crate::auth::Tokens {
        crate::auth::Tokens::new(self.key.clone())
    }

    /// The app's config, registered with `App::config` in `src/app.rs`.
    pub fn config<C: Send + Sync + 'static>(&self) -> &C {
        self.get::<C>()
    }

    /// A value registered with `App::provide`, if there is one.
    pub fn try_get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.provided.get(&std::any::TypeId::of::<T>()).and_then(|value| value.downcast_ref::<T>())
    }

    /// A value registered with `App::provide` (or `App::config`) in `src/app.rs`, e.g. an
    /// API client. Panics, naming the type, if it was never registered.
    pub fn get<T: Send + Sync + 'static>(&self) -> &T {
        self.provided.get(&std::any::TypeId::of::<T>()).and_then(|value| value.downcast_ref::<T>()).unwrap_or_else(
            || panic!("`{}` isn't registered; add `app.provide(..)` in src/app.rs", std::any::type_name::<T>()),
        )
    }
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
    assets: Option<Assets>,
    tools: Option<ToolsAccess>,
    stop_on_pending_migrations: bool,
}

/// Decides who may use `/kiln` in production: `(who, seed)` for an allowed viewer, where
/// `seed` (their sign-in token) keys the control forms' token.
type ToolsAccess = Arc<dyn Fn(Session, Db) -> BoxFuture<'static, Result<Option<(String, String)>>> + Send + Sync>;

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
    db: Db,
    sign_ins: Arc<[Arc<dyn SignIn>]>,
    assets: Option<Assets>,
    tools: Option<ToolsAccess>,
    /// The app's context, for `/kiln` pages that run app code (mail previews).
    ctx: Ctx,
    stop_on_pending_migrations: bool,
}

#[derive(Clone)]
struct RouteState {
    ctx: Ctx,
    key: SecretKey,
    sign_ins: Arc<[Arc<dyn SignIn>]>,
}

/// Largest form body kiln buffers for CSRF and method-override checks.
const FORM_LIMIT: usize = 2 * 1024 * 1024;

impl App {
    /// An app with a random session key, so sessions don't survive a restart.
    /// `kiln::Cli` sets a persistent key.
    pub fn new(db: Db) -> Self {
        let key = SecretKey::generate();
        Self {
            ctx: Ctx { db, key: key.clone(), provided: Arc::default() },
            key,
            sign_ins: vec![],
            assets: None,
            tools: None,
            stop_on_pending_migrations: false,
        }
    }

    /// The app's directory, so `assets/` and `public/` are served. `kiln::Cli` and
    /// `#[kiln::test]` set it.
    pub fn root(mut self, root: impl AsRef<std::path::Path>) -> Self {
        self.assets = Some(Assets::at(root.as_ref()));
        self
    }

    /// Registers the app's config so actions read it with `ctx.config::<Config>()`.
    pub fn config<C: Send + Sync + 'static>(self, config: C) -> Self {
        self.provide(config)
    }

    /// Opens the `/kiln` tools (jobs, schedules, batches, mail, ...) in production to signed-in
    /// `T`s that `allow` accepts: `app.kiln_tools_for::<User>(|user| user.admin)`. Everyone else
    /// gets a 404. In development the tools are open regardless. Control actions taken in
    /// production are logged with who took them.
    pub fn kiln_tools_for<T: Record>(mut self, allow: fn(&T) -> bool) -> Self {
        self.tools = Some(Arc::new(move |session: Session, db: Db| {
            Box::pin(async move {
                let Some(token) = session.sign_in_token::<T>() else { return Ok(None) };
                let viewer = crate::auth::find_signed_in::<T>(&db, &token).await?.filter(|record| allow(record));
                Ok(viewer.map(|record| (format!("{} #{}", T::TABLE, record.id().get()), token)))
            })
        }));
        self
    }

    /// In development, every page shows "migrations are pending" (with a button that runs
    /// them) until they're applied. Off by default: pages load as usual, and only an error from
    /// the database offers to migrate.
    pub fn stop_on_pending_migrations(mut self) -> Self {
        self.stop_on_pending_migrations = true;
        self
    }

    /// Caps how many jobs from `queue` run at once, across every worker process:
    /// `app.queue_limit("imports", 2)`. Other queues are limited only by the workers.
    pub fn queue_limit(self, queue: &'static str, max_running: i64) -> Self {
        let mut limits = self.ctx.try_get::<crate::jobs::QueueLimits>().cloned().unwrap_or_default();
        limits.0.retain(|(q, _)| *q != queue);
        limits.0.push((queue, max_running));
        self.provide(limits)
    }

    /// The app's recurring jobs: `app.schedule(schedule::schedule)` with the function in
    /// `src/schedule.rs`. Job workers run them.
    pub fn schedule(self, build: fn(crate::Schedule) -> crate::Schedule) -> Self {
        self.provide(build(crate::Schedule::default()))
    }

    /// Registers a value (an API client, a service) that actions read with `ctx.get::<T>()`.
    /// One value per type, built once at boot.
    pub fn provide<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        let mut provided = (*self.ctx.provided).clone();
        provided.insert(std::any::TypeId::of::<T>(), Arc::new(value));
        self.ctx.provided = Arc::new(provided);
        self
    }

    pub fn secret_key(mut self, key: SecretKey) -> Self {
        self.ctx.key = key.clone();
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
        crate::storage::set_signing_key(&self.key);
        let sign_ins: Arc<[Arc<dyn SignIn>]> = self.sign_ins.clone().into();
        let route_state = RouteState { ctx: self.ctx.clone(), key: self.key.clone(), sign_ins: sign_ins.clone() };
        let routes = routes()
            .into_iter()
            .fold(axum::Router::<Ctx>::new(), |router, route| (route.mount)(router))
            .route_layer(middleware::from_fn_with_state(route_state, route_checks))
            .with_state(self.ctx.clone());
        let shared = Arc::new(Shared {
            key: self.key.clone(),
            db: self.ctx.db.clone(),
            sign_ins,
            assets: self.assets.clone(),
            tools: self.tools.clone(),
            ctx: self.ctx.clone(),
            stop_on_pending_migrations: self.stop_on_pending_migrations,
        });
        // Method override must happen before routing, so request handling wraps the whole
        // router instead of each route.
        axum::Router::new().fallback_service(routes).layer(middleware::from_fn_with_state(shared, handle_request))
    }

    pub async fn serve(self, addr: &str) -> Result<()> {
        // So /kiln is right even when jobs run in another process.
        crate::jobs::sync_app(&self.ctx).await?;
        crate::jobs::listen_for_shutdown();
        crate::jobs::exit_with_kiln_dev();
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                let port = addr.rsplit(':').next().unwrap_or(addr);
                Error::BadRequest(format!(
                    "{addr} is already in use. Something else is listening there; see what with `lsof -nP -iTCP:{port} -sTCP:LISTEN`, or run on another port (`kiln dev --port 4000`)"
                ))
            } else {
                e.into()
            }
        })?;
        println!("{}\n\nkiln listening on http://{addr}", routes_table());
        // SIGTERM stops new connections and lets requests finish (open streams end).
        axum::serve(listener, self.router().into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(crate::jobs::shutdown_requested())
            .await?;
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
        Some(RequestScope { key: Some(state.key.clone()), ..RequestScope::default() })
    } else {
        let session = request.extensions().get::<Session>().cloned().unwrap_or_default();
        let mutating = !matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
        let csrf_ok = !mutating
            || {
                let sent = request.extensions().get::<SubmittedCsrf>().and_then(|s| s.0.as_deref());
                matches!((sent, session.existing_csrf_token()), (Some(sent), Some(expected)) if tokens_match(sent, &expected))
            };
        let frame = Turbo::from_parts_of(&request).frame().is_some();
        csrf_ok.then(|| {
            let flash = session.take_flash();
            RequestScope { session, flash, cache: Default::default(), frame, key: Some(state.key.clone()) }
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
    let page = response.extensions_mut().remove::<PendingRender>();
    let stream = response.extensions_mut().remove::<PendingStream>();
    if page.is_none() && stream.is_none() {
        return response;
    }
    let mut used = std::collections::HashSet::new();
    for render in page.iter().chain(stream.iter().flat_map(|s| s.0.contents())) {
        match render.variables() {
            Ok(vars) => used.extend(vars),
            Err(e) => return e.into_response(),
        }
    }
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
    let (rendered, content_type) = match (page, stream) {
        (Some(page), _) if scope.frame => (page.render_without_layout(), "text/html; charset=utf-8"),
        (Some(page), _) => (page.render(), "text/html; charset=utf-8"),
        (None, Some(stream)) => (stream.0.render(), turbo::STREAM_MIME),
        (None, None) => unreachable!("checked above"),
    };
    match rendered {
        Ok(html) => {
            let (mut parts, _) = response.into_parts();
            parts.headers.remove(header::CONTENT_LENGTH);
            parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            Response::from_parts(parts, Body::from(html))
        }
        Err(e) => e.into_response(),
    }
}

/// Who is viewing `/kiln`, or `None` when the tools are off for this request.
async fn tools_viewer(shared: &Shared, headers: &axum::http::HeaderMap) -> Option<crate::devtools::Viewer> {
    if !crate::config::is_production() {
        return Some(crate::devtools::Viewer::new(&shared.key, "development".into(), "development"));
    }
    let access = shared.tools.as_ref()?;
    let session = cookie_in(headers, COOKIE).and_then(|c| shared.key.verify(c)).map(Session::from_data)?;
    match access(session, shared.db.clone()).await {
        Ok(Some((who, seed))) => Some(crate::devtools::Viewer::new(&shared.key, who, &seed)),
        Ok(None) => None,
        Err(e) => {
            tracing::error!(target: "kiln::tools", error = %e, "checking /kiln access failed");
            None
        }
    }
}

fn cookie<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    cookie_in(&parts.headers, name)
}

fn cookie_in<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
}

/// In development, the migrations not yet applied (`version_name`); none in production.
async fn pending_migration_names(db: &Db) -> Vec<String> {
    if crate::config::is_production() {
        return vec![];
    }
    let Some(root) = crate::config::root() else { return vec![] };
    match db.pending_migrations(&root.join("db/migrations")).await {
        Ok(pending) => pending.iter().map(|m| format!("{}_{}", m.version, m.name)).collect(),
        Err(_) => vec![],
    }
}

/// Says which migrations are waiting and offers to run them.
fn offer_migrations(report: &mut ErrorReport, key: &SecretKey, pending: &[String], back: &str) {
    report.hints.insert(
        0,
        format!("{} migration(s) haven't been applied: {}. Run `kiln db migrate`.", pending.len(), pending.join(", ")),
    );
    let viewer = crate::devtools::Viewer::new(key, "development".into(), "development");
    report.actions.push(crate::error_page::ResolveAction {
        label: "Run migrations".into(),
        path: "/kiln/resolve/migrate".into(),
        token: viewer.token().to_owned(),
        back: back.to_owned(),
    });
}

/// The text fields of a multipart body (`_method`, `_csrf`, ..); files are skipped.
async fn multipart_text_fields(bytes: axum::body::Bytes, boundary: &str) -> Vec<(String, String)> {
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut fields = vec![];
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_owned();
        if field.file_name().is_some() || !name.starts_with('_') {
            continue;
        }
        if let Ok(text) = field.text().await {
            fields.push((name, text));
        }
    }
    fields
}

fn too_large() -> Response {
    let mut response = Error::BadRequest(format!(
        "the upload is larger than MAX_UPLOAD_SIZE ({} bytes)",
        crate::storage::max_upload_size()
    ))
    .into_response();
    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    response
}

/// Logs one line per request under the `kiln::request` target and runs the browser
/// pipeline: session, flash, CSRF, `_method` override, and error pages (HTML for
/// browsers, Markdown otherwise).
async fn handle_request(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    // `/kiln` tools: open in development; in production only for viewers `kiln_tools_for`
    // allows (everyone else falls through to an ordinary 404).
    let path = request.uri().path();
    let tools_path = (path == "/kiln" || path.starts_with("/kiln/"))
        && !path.starts_with("/kiln/streams/")
        && !path.starts_with(crate::storage::FILES_PATH)
        && !path.starts_with(crate::storage::VARIANTS_PATH)
        && path != crate::storage::UPLOADS_PATH;
    let viewer = if tools_path { tools_viewer(&shared, request.headers()).await } else { None };
    if *request.method() == Method::POST
        && let Some(viewer) = viewer.clone()
    {
        return crate::devtools::act(&shared.ctx, viewer, request).await;
    }
    // Static files skip sessions and logging: they never set cookies.
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        let path = request.uri().path();
        let file = if let Some(name) = path.strip_prefix("/assets/") {
            shared.assets.as_ref().and_then(|a| a.serve_asset(name))
        } else if path == crate::dev_reload::PATH && crate::dev_reload::enabled() {
            shared.assets.as_ref().map(crate::dev_reload::serve)
        } else if path.starts_with(crate::storage::FILES_PATH) {
            crate::storage::serve(&shared.ctx, &shared.key, path).await
        } else if path.starts_with(crate::storage::VARIANTS_PATH) {
            crate::storage::serve_variant(&shared.ctx, &shared.key, path).await
        } else if path.starts_with("/kiln/streams/") {
            let last_event_id = request.headers().get("last-event-id").and_then(|v| v.to_str().ok());
            realtime::serve(shared.db.clone(), &shared.key, path, last_event_id).await
        } else if path == "/kiln" || path.starts_with("/kiln/") {
            let page = match viewer {
                Some(viewer) => crate::devtools::serve(&shared.ctx, viewer, path, request.uri().query()).await,
                None => None,
            };
            page.or_else(|| assets::serve_kiln(path))
        } else {
            None
        };
        if let Some(file) = file {
            return file;
        }
    }
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
    session.set_client(header_str(&parts, header::USER_AGENT).to_owned(), client_ip(&parts));

    // Form bodies are buffered so `_method` and `_csrf` can be read before routing.
    let mutating = !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
    let content_type = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let is_form = content_type.starts_with("application/x-www-form-urlencoded");
    let boundary =
        multer::parse_boundary(content_type).ok().filter(|_| content_type.starts_with("multipart/form-data"));
    let mut fields: Vec<(String, String)> = vec![];
    let mut uploaded: Option<(axum::body::Bytes, String)> = None;
    let body = if mutating && is_form {
        match axum::body::to_bytes(body, FORM_LIMIT).await {
            Ok(bytes) => {
                fields = serde_urlencoded::from_bytes(&bytes).unwrap_or_default();
                Body::from(bytes)
            }
            Err(_) => Body::empty(),
        }
    } else if let (true, Some(boundary)) = (mutating, boundary) {
        // Uploads: buffered up to MAX_UPLOAD_SIZE (the form extractor reads them again).
        match axum::body::to_bytes(body, crate::storage::max_upload_size() + FORM_LIMIT).await {
            Ok(bytes) => {
                fields = multipart_text_fields(bytes.clone(), &boundary).await;
                uploaded = Some((bytes.clone(), boundary));
                Body::from(bytes)
            }
            Err(_) => return too_large(),
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
    // The rich text editor's uploads: same-site browsers only, like any form.
    if method == Method::POST && path == crate::storage::UPLOADS_PATH {
        let csrf_ok = matches!((sent, session.existing_csrf_token()), (Some(sent), Some(expected)) if tokens_match(sent, &expected));
        let response = crate::storage::direct_upload(&shared.ctx, csrf_ok, uploaded).await;
        tracing::info!(target: "kiln::request", method = %method, path, status = response.status().as_u16(), elapsed = ?started.elapsed());
        return response;
    }
    parts.extensions.insert(SubmittedCsrf(sent.map(str::to_owned)));
    parts.extensions.insert(session.clone());
    let blocked = if shared.stop_on_pending_migrations { pending_migration_names(&shared.db).await } else { vec![] };
    let mut response = if blocked.is_empty() {
        next.run(Request::from_parts(parts, body)).await
    } else {
        Error::PendingMigrations(blocked).into_response()
    };

    let api = response.extensions().get::<PipelineName>().is_some_and(|p| p.0 == "api");
    if let Some(flash) = response.extensions_mut().remove::<Flash>()
        && !api
    {
        session.set_flash(&flash);
    }
    let action = response.extensions().get::<ActionName>().map(|a| a.0);
    // No route matched a GET: try `public/`.
    if action.is_none()
        && response.status() == StatusCode::NOT_FOUND
        && matches!(method, Method::GET | Method::HEAD)
        && let Some(file) = shared.assets.as_ref().and_then(|a| a.serve_public(&path))
    {
        return file;
    }
    let status = response.status().as_u16();
    let elapsed = started.elapsed();
    // In development, a database error with migrations waiting is almost always that.
    let database_error =
        response.extensions().get::<ErrorReport>().is_some_and(|r| matches!(r.code, "K1201" | "K1203"));
    let pending = if database_error { pending_migration_names(&shared.db).await } else { vec![] };
    let mut report = response
        .extensions_mut()
        .remove::<ErrorReport>()
        .or_else(|| ErrorReport::for_routing(response.status(), method.as_str(), &path));
    if let Some(report) = report.as_mut()
        && !pending.is_empty()
    {
        offer_migrations(report, &shared.key, &pending, if method == Method::GET { &full_path } else { "/" });
    }

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

fn header_str<'a>(parts: &'a axum::http::request::Parts, name: header::HeaderName) -> &'a str {
    parts.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The client's address: the first `X-Forwarded-For` hop (set by a proxy in production),
/// else the connection's peer.
fn client_ip(parts: &axum::http::request::Parts) -> String {
    if let Some(forwarded) = parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        return forwarded.split(',').next().unwrap_or("").trim().to_owned();
    }
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip().to_string())
        .unwrap_or_default()
}
