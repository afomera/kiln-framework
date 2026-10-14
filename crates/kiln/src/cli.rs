//! The command line built into every kiln app binary. `kiln db ...` and `kiln routes`
//! delegate here, because only the compiled app knows its records and routes.
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() {
//!     kiln::Cli::new(env!("CARGO_MANIFEST_DIR")).seed(blog::seed).run().await
//! }
//! ```

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use crate::{
    App, Db, Error, Result, SecretKey, Snapshot, diff, migrate::records, routes, routes_table, write_migration,
};

type Seed = Box<dyn FnOnce(Db) -> Pin<Box<dyn Future<Output = Result>>>>;
type Configure = Box<dyn FnOnce(App) -> Result<App>>;

pub struct Cli {
    root: PathBuf,
    seed: Option<Seed>,
    configure: Option<Configure>,
}

const HELP: &str = "\
Usage: <app> [command]

Commands:
  serve               Run the web server and a job worker (default). PORT sets the port;
                      WORKERS=0 leaves jobs to a separate `work` process.
  work                Run background jobs only (no web server). WORKERS sets concurrency.
  jobs                Job counts, queues and recent failures
  jobs retry [id]     Queue failed jobs (one, or all) to run again
  jobs cancel <id>    Remove a waiting or failed job
  jobs run <id>       Make a delayed job due now
  jobs discard-failed Delete every failed job
  jobs pause <queue>  Stop taking jobs from a queue; jobs resume <queue> undoes it
  schedules           Recurring jobs; schedules pause|resume|run <job name>
  batches             Recent batches; batches cancel <id>
  health              Exit 0 if the running server answers GET /up (for HEALTHCHECK)
  introspect          The app as JSON: routes, records, jobs, schedules, notifications, lints
  lint [--list]       Run the app's own `kiln check` rules (src/lints/), or list them
  db migrate          Apply pending migrations
  db diff <name>      Write a migration for changes to the record structs
  db undo             Delete the newest migration and restore the snapshot from before it
  db squash [--before <year>]
                      One migration file per past year (default: every year before this one)
  db status [--json]  Pending migrations and schema drift
  db seed             Run the app's seed function
  db reset            Recreate the development database, migrate and seed
  secrets ...         Encrypted secrets: init | show | set KEY VALUE | unset KEY | edit
  routes [--json]     The route table
  manifest            Routes and records as JSON

For agents (`kiln mcp`; debug builds, not in production), all JSON:
  request <METHOD> <path> [--as User:1|<email>] [--form k=v].. [--json BODY] [--accept html|json|turbo] [--follow]
                      A request through the app, with the SQL it ran and the jobs and mail it caused
  db query \"<SQL>\" [--limit N] [--database queue|cache|streams]
                      Read-only SQL
  mail [--limit N]    The newest mail in the outbox
  jobs --json         Job counts, queues and failures
";

impl Cli {
    /// `root` is the app crate's directory: pass `env!("CARGO_MANIFEST_DIR")`.
    ///
    /// A deployed binary reads its files (templates, assets, `public/`, migrations) from
    /// `KILN_ROOT` instead, since the directory it was compiled in isn't there.
    pub fn new(root: &str) -> Self {
        let root = crate::config::root_override().unwrap_or_else(|| PathBuf::from(root));
        Self { root, seed: None, configure: None }
    }

    /// The app's templates, assets, migrations and secrets as compiled into the binary:
    /// `.files(kiln::embedded_files!())`. Release builds then need nothing beside them, and
    /// run with the current directory as the app's root (where `storage/` is written).
    /// Debug builds embed nothing; `KILN_ROOT` makes a release build read from disk instead.
    pub fn files(mut self, embedded: crate::Embedded) -> Self {
        if !embedded.is_empty() && crate::config::root_override().is_none() {
            crate::files::install(embedded);
            if let Ok(cwd) = std::env::current_dir() {
                self.root = cwd;
            }
        }
        self
    }

    /// Sample data for `db seed` and `db reset`.
    pub fn seed<F, Fut>(mut self, seed: F) -> Self
    where
        F: FnOnce(Db) -> Fut + 'static,
        Fut: Future<Output = Result> + 'static,
    {
        self.seed = Some(Box::new(move |db| Box::pin(seed(db))));
        self
    }

    /// App settings, e.g. `|app| app.sign_in_path("/session/new")`.
    pub fn configure(mut self, configure: impl FnOnce(App) -> Result<App> + 'static) -> Self {
        self.configure = Some(Box::new(configure));
        self
    }

    /// Runs the command from the process arguments and exits non-zero on failure.
    pub async fn run(self) {
        crate::config::set_root(&self.root);
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Err(e) = self.dispatch(&args.iter().map(String::as_str).collect::<Vec<_>>()).await {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }

    fn migrations_dir(&self) -> PathBuf {
        self.root.join("db/migrations")
    }

    fn snapshot_path(&self) -> PathBuf {
        self.root.join("db/snapshot.json")
    }

    /// The snapshot as it was before migration `version`, kept for `db undo`. Local only
    /// (`db/.kiln/` is gitignored): undo is for a migration you just generated.
    fn snapshot_backup(&self, version: &str) -> PathBuf {
        self.root.join(format!("db/.kiln/snapshot-before-{version}.json"))
    }

    /// An error when the snapshot was written with a migration that's no longer on disk
    /// (deleted by hand): diffing from it would leave that change out.
    fn check_snapshot_current(&self, snapshot: &Snapshot) -> Result {
        let Some(version) = &snapshot.migration else { return Ok(()) };
        let on_disk = crate::migrate::load_migrations(&self.migrations_dir())?
            .iter()
            .any(|m| m.parts().iter().any(|p| &p.version == version));
        if on_disk {
            return Ok(());
        }
        let fix = if self.snapshot_backup(version).exists() {
            "run `kiln db undo` to restore the snapshot from before it".to_owned()
        } else {
            "restore the file, or db/snapshot.json from version control".to_owned()
        };
        Err(Error::BadRequest(format!(
            "db/snapshot.json was written with migration {version}, which is gone; {fix}, then `kiln db diff` again"
        )))
    }

    /// The backend the app's migrations are written for (`db/snapshot.json`).
    fn backend(&self) -> &'static dyn crate::dialect::Dialect {
        Snapshot::load(&self.snapshot_path()).map(|s| s.dialect()).unwrap_or(&crate::dialect::Sqlite)
    }

    /// `DATABASE_URL`, else the backend's default: SQLite in `storage/` beside uploaded files
    /// (`storage/development.sqlite3`, `storage/production.sqlite3`), so one directory (one
    /// volume, in a container) holds all the app's data; or `postgres://localhost/<app>_development`.
    fn database_url(&self) -> String {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            return url;
        }
        match self.backend().name() {
            "postgres" => format!("postgres://localhost/{}_development", app_name(&self.root)),
            _ => format!("sqlite://{}", self.sqlite_path().display()),
        }
    }

    /// The default SQLite file, in `storage/` beside uploaded files.
    fn sqlite_path(&self) -> PathBuf {
        let name = if crate::config::is_production() { "production" } else { "development" };
        self.root.join(format!("storage/{name}.sqlite3"))
    }

    /// The app's database, plus separate ones for Kiln's queue, cache and broadcasts when
    /// `QUEUE_DATABASE_URL`, `CACHE_DATABASE_URL` or `STREAMS_DATABASE_URL` name them.
    async fn db(&self) -> Result<Db> {
        if let Some(dir) = self.sqlite_path().parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut db = Db::connect_or_create(&self.database_url()).await?;
        for (role, name) in [
            (crate::db::Role::Queue, "QUEUE_DATABASE_URL"),
            (crate::db::Role::Cache, "CACHE_DATABASE_URL"),
            (crate::db::Role::Streams, "STREAMS_DATABASE_URL"),
        ] {
            if let Some(url) = crate::config::value(name).filter(|url| !url.is_empty()) {
                let separate = Db::connect_or_create(&url).await?;
                if separate.dialect().name() != db.dialect().name() {
                    return Err(Error::Config(format!(
                        "{name} is {}, but the app's database is {}; use the same backend for both",
                        separate.dialect().name(),
                        db.dialect().name()
                    )));
                }
                db = db.with_role(role, separate);
            }
        }
        Ok(db)
    }

    async fn dispatch(self, args: &[&str]) -> Result {
        match args {
            [] | ["serve"] => self.serve().await,
            ["health"] => health(),
            // For agents (`kiln mcp`): development builds only, never in production.
            #[cfg(debug_assertions)]
            ["request", rest @ ..] => {
                refuse_in_production("request")?;
                // stdout is the JSON; the request's own log line would corrupt it.
                use tracing::instrument::WithSubscriber;
                let app = self.app().await?;
                let quiet = tracing::subscriber::NoSubscriber::default();
                print_json(&crate::agent::request(&app, rest).with_subscriber(quiet).await?);
                Ok(())
            }
            #[cfg(debug_assertions)]
            ["db", "query", rest @ ..] => {
                refuse_in_production("db query")?;
                print_json(&crate::agent::query(&self.db().await?, rest).await?);
                Ok(())
            }
            #[cfg(debug_assertions)]
            ["mail", rest @ ..] => {
                print_json(&crate::agent::outbox(&self.db().await?, rest).await?);
                Ok(())
            }
            #[cfg(debug_assertions)]
            ["templates", "--variables"] => {
                let root: &'static str =
                    Box::leak(self.root.join("templates").to_string_lossy().into_owned().into_boxed_str());
                print_json(&crate::view::template_variables(root));
                Ok(())
            }
            #[cfg(debug_assertions)]
            ["jobs", "--json"] => {
                print_json(&crate::agent::jobs(&self.db().await?).await?);
                Ok(())
            }
            // The app's own `kiln check` rules; development builds only (see `kiln::lint`).
            #[cfg(debug_assertions)]
            ["lint"] => {
                println!("{}", crate::lint::run(&self.root));
                Ok(())
            }
            #[cfg(debug_assertions)]
            ["lint", "--list"] => {
                println!("{}", serde_json::to_string_pretty(&crate::lint::list()).expect("lints serialize"));
                Ok(())
            }
            ["introspect"] => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&self.introspect().await?).expect("introspection serializes")
                );
                Ok(())
            }
            ["db", "migrate"] => {
                let applied = self.db().await?.migrate(&self.migrations_dir()).await?;
                if applied.is_empty() {
                    println!("No pending migrations.");
                }
                for m in applied {
                    println!("applied {}_{}", m.version, m.name);
                }
                Ok(())
            }
            ["db", "diff", name] => self.diff(name),
            ["db", "undo"] => self.undo().await,
            ["db", "squash"] => self.squash(&jiff::Timestamp::now().strftime("%Y").to_string()).await,
            ["db", "squash", "--before", year] => self.squash(year).await,
            ["db", "status", rest @ ..] => self.status(rest.contains(&"--json")).await,
            ["db", "seed"] => self.run_seed().await,
            ["db", "reset"] => self.reset().await,
            ["routes"] => {
                println!("{}", routes_table());
                Ok(())
            }
            ["routes", "--json"] | ["manifest"] => {
                println!("{}", serde_json::to_string_pretty(&manifest()).expect("manifest serializes"));
                Ok(())
            }
            ["secrets", rest @ ..] => self.secrets(rest),
            ["work"] => {
                let app = self.app().await?;
                tracing::info!(target: "kiln::jobs", workers = workers(), "working");
                crate::jobs::work(app.ctx().clone(), workers().max(1)).await;
                Ok(())
            }
            ["jobs"] => {
                let db = self.db().await?;
                let counts = db.job_counts().await?;
                if counts.is_empty() {
                    println!("No jobs.");
                }
                for (status, count) in counts {
                    println!("{status:>8}  {count}");
                }
                for (queue, paused, queued, running, failed) in db.queues().await? {
                    let paused = if paused { "  (paused)" } else { "" };
                    println!("queue {queue}: {queued} waiting, {running} running, {failed} failed{paused}");
                }
                for (id, name, attempts, error) in db.failed_jobs().await? {
                    println!("\nfailed #{id} {name} after {attempts} attempt(s):\n  {error}");
                }
                Ok(())
            }
            ["jobs", "cancel", id] => {
                let done = self.db().await?.cancel_job(parse_id(id)?).await?;
                println!("{}", if done { "Removed." } else { "No waiting or failed job with that id." });
                Ok(())
            }
            ["jobs", "run", id] => {
                let done = self.db().await?.run_job_now(parse_id(id)?).await?;
                println!("{}", if done { "Due now." } else { "No waiting job with that id." });
                Ok(())
            }
            ["jobs", "discard-failed"] => {
                println!("Discarded {} failed job(s).", self.db().await?.discard_failed_jobs().await?);
                Ok(())
            }
            ["jobs", "pause", queue] => {
                self.db().await?.pause_queue(queue).await?;
                println!("Paused `{queue}`: running jobs finish, new ones wait.");
                Ok(())
            }
            ["jobs", "resume", queue] => {
                self.db().await?.resume_queue(queue).await?;
                println!("Resumed `{queue}`.");
                Ok(())
            }
            ["schedules"] => {
                let db = self.app().await?.ctx().db.clone();
                let schedules = db.schedules().await?;
                if schedules.is_empty() {
                    println!("No schedules. Declare them in src/schedule.rs and register with `app.schedule(..)`.");
                }
                for s in schedules {
                    let next = if s.paused { "paused".to_owned() } else { s.next_run_at.to_string() };
                    println!("{:<28} {:<24} next {next}", s.name, s.when);
                }
                Ok(())
            }
            ["schedules", verb @ ("pause" | "resume" | "run"), name] => {
                let db = self.db().await?;
                let found = match *verb {
                    "run" => db.run_schedule_now(name).await?.is_some(),
                    verb => db.set_schedule_paused(name, verb == "pause").await?,
                };
                if !found {
                    return Err(Error::BadRequest(format!("no schedule named `{name}` (see `kiln schedules`)")));
                }
                println!(
                    "{} {name}.",
                    match *verb {
                        "run" => "Enqueued",
                        "pause" => "Paused",
                        _ => "Resumed",
                    }
                );
                Ok(())
            }
            ["batches"] => {
                for b in self.db().await?.batches(25).await? {
                    println!(
                        "#{:<5} {:<10} {}/{} done, {} failed  {}",
                        b.id.0,
                        b.status,
                        b.succeeded + b.failed + b.cancelled,
                        b.total,
                        b.failed,
                        b.description
                    );
                }
                Ok(())
            }
            ["batches", "cancel", id] => {
                let done = crate::batch::BatchId(parse_id(id)?).cancel(&self.db().await?).await?;
                println!("{}", if done { "Cancelled." } else { "That batch had already ended." });
                Ok(())
            }
            ["jobs", "retry", rest @ ..] => {
                let id = match rest {
                    [] => None,
                    [id] => Some(id.parse().map_err(|_| Error::BadRequest(format!("`{id}` isn't a job id")))?),
                    _ => return Err(Error::BadRequest("usage: jobs retry [id]".into())),
                };
                let queued = self.db().await?.retry_jobs(id).await?;
                println!("Queued {queued} failed job(s) to run again.");
                Ok(())
            }
            ["help" | "--help" | "-h"] => {
                print!("{HELP}");
                Ok(())
            }
            _ => Err(Error::BadRequest(format!("unknown command `{}`\n\n{HELP}", args.join(" ")))),
        }
    }

    async fn serve(self) -> Result {
        let app = self.app().await?;
        let worker = (workers() > 0).then(|| tokio::spawn(crate::jobs::work(app.ctx().clone(), workers())));
        let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
        // Containers and hosts reach the app from outside; development stays local.
        let host = std::env::var("HOST")
            .unwrap_or_else(|_| if crate::config::is_production() { "0.0.0.0".into() } else { "127.0.0.1".into() });
        app.serve(&format!("{host}:{port}")).await?;
        // Shutting down: let running jobs finish (or be released) before exiting.
        if let Some(worker) = worker {
            let _ = worker.await;
        }
        Ok(())
    }

    /// The configured app, refusing to start with pending migrations.
    /// What the binary knows about the app (`kiln introspect` adds what the source says).
    /// Built against an in-memory database, so it needs no database of its own.
    async fn introspect(self) -> Result<serde_json::Value> {
        use serde_json::json;
        let backend = self.backend().name();
        let app_name = app_name(&self.root);
        let mut app = App::new(Db::memory().await?).root(&self.root);
        if let Some(configure) = self.configure {
            app = configure(app)?;
        }
        let routes: Vec<_> = routes()
            .iter()
            .map(|r| {
                let args: Vec<_> = r.args.iter().map(|(name, ty)| json!({ "name": name, "type": ty })).collect();
                let wrapped = |wrapper: &str| {
                    r.args.iter().find_map(|(_, ty)| {
                        ty.strip_prefix(&format!("{wrapper}<")).and_then(|t| t.strip_suffix('>')).map(str::to_owned)
                    })
                };
                let params: Vec<_> = r
                    .args
                    .iter()
                    .filter_map(|(name, ty)| {
                        let inner = ty.strip_prefix("Param<")?.strip_suffix('>')?;
                        Some(json!({ "name": name, "type": inner }))
                    })
                    .collect();
                let optional_actor =
                    wrapped("Option").and_then(|t| t.strip_prefix("Current<")?.strip_suffix('>').map(str::to_owned));
                json!({
                    "method": r.method,
                    "path": r.path,
                    "action": r.name(),
                    "file": r.file,
                    "line": r.line,
                    "pipeline": r.pipeline,
                    "params": params,
                    "signed_in": wrapped("Current").or(optional_actor.clone()),
                    "sign_in_required": wrapped("Current").is_some(),
                    "form": wrapped("Form"),
                    "json_body": wrapped("Json"),
                    "returns": r.returns,
                    "args": args,
                })
            })
            .collect();
        let records: Vec<_> = records()
            .iter()
            .map(|r| {
                let schema = (r.schema)();
                let columns: Vec<_> = schema
                    .columns
                    .iter()
                    .map(|c| {
                        json!({
                            "name": c.name,
                            "type": c.sql_type,
                            "nullable": c.nullable,
                            "primary_key": c.primary_key,
                            "unique": c.unique,
                            "indexed": c.indexed,
                            "references": c.references,
                            "variants": c.variants,
                            "attachment": c.attachment,
                            "rich_text": c.rich_text,
                        })
                    })
                    .collect();
                let relations: Vec<_> = r
                    .relations
                    .iter()
                    .map(|(kind, name, target)| json!({ "kind": kind, "name": name, "target": target }))
                    .collect();
                json!({ "name": r.name, "table": r.table, "columns": columns, "relations": relations })
            })
            .collect();
        let mut jobs: Vec<_> = inventory::iter::<crate::jobs::JobDef>
            .into_iter()
            .map(|j| {
                json!({
                    "name": j.name,
                    "queue": j.queue,
                    "attempts": j.attempts,
                    "priority": j.priority,
                    "timeout_seconds": j.timeout,
                    "unique_for_seconds": j.unique_for,
                })
            })
            .collect();
        jobs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let schedules: Vec<_> = app
            .ctx()
            .try_get::<crate::Schedule>()
            .map(|s| {
                s.entries
                    .iter()
                    .map(|e| json!({ "job": e.name, "when": e.when.describe(), "queue": e.queue }))
                    .collect()
            })
            .unwrap_or_default();
        let mut notifications: Vec<_> = inventory::iter::<crate::notify::NotificationDef>
            .into_iter()
            .map(|n| {
                let optional: Vec<_> = n.optional.iter().map(|c| format!("{c:?}").to_lowercase()).collect();
                json!({ "name": n.name, "description": n.description, "to": (n.to)(), "optional": optional })
            })
            .collect();
        notifications.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let mut previews: Vec<&str> = inventory::iter::<crate::mail::PreviewDef>.into_iter().map(|p| p.name).collect();
        previews.sort();
        Ok(json!({
            "kiln": env!("CARGO_PKG_VERSION"),
            "app": app_name,
            "backend": backend,
            "routes": routes,
            "records": records,
            "jobs": jobs,
            "schedules": schedules,
            "notifications": notifications,
            "mail_previews": previews,
            "lints": crate::lint::list(),
        }))
    }

    async fn app(self) -> Result<App> {
        let db = self.db().await?;
        let pending = db.pending_migrations(&self.migrations_dir()).await?;
        if !pending.is_empty() {
            let names: Vec<String> = pending.iter().map(|m| format!("  {}_{}", m.version, m.name)).collect();
            return Err(Error::BadRequest(format!(
                "{} pending migration(s); run `kiln db migrate` first:\n{}",
                pending.len(),
                names.join("\n")
            )));
        }
        let mut app = App::new(db).secret_key(self.secret_key()?).root(&self.root);
        if let Some(configure) = self.configure {
            app = configure(app)?;
        }
        Ok(app)
    }

    /// `KILN_SECRET_KEY`, or in debug builds a key kept in `tmp/development_secret`.
    fn secret_key(&self) -> Result<SecretKey> {
        if let Ok(key) = std::env::var("KILN_SECRET_KEY") {
            if key.len() < 32 {
                return Err(Error::BadRequest("KILN_SECRET_KEY must be at least 32 characters".into()));
            }
            return Ok(SecretKey::new(key.as_bytes()));
        }
        if !cfg!(debug_assertions) {
            return Err(Error::BadRequest("set KILN_SECRET_KEY (64+ random characters) in production".into()));
        }
        let path = self.root.join("tmp/development_secret");
        let key = match std::fs::read(&path) {
            Ok(bytes) if bytes.len() >= 32 => bytes,
            _ => {
                use rand::RngCore;
                let mut bytes = [0u8; 64];
                rand::thread_rng().fill_bytes(&mut bytes);
                std::fs::create_dir_all(self.root.join("tmp"))?;
                std::fs::write(&path, bytes)?;
                bytes.to_vec()
            }
        };
        Ok(SecretKey::new(&key))
    }

    fn diff(&self, name: &str) -> Result {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return Err(Error::BadRequest(format!("migration names are snake_case, e.g. create_posts (got `{name}`)")));
        }
        let old = Snapshot::load(&self.snapshot_path())?;
        self.check_snapshot_current(&old)?;
        // The backend is fixed by the first migration: the snapshot's, else DATABASE_URL's.
        let dialect = match (&old.backend, std::env::var("DATABASE_URL")) {
            (Some(_), Ok(url)) if crate::dialect::for_url(&url)?.name() != old.dialect().name() => {
                return Err(Error::Config(format!(
                    "this app's migrations are written for {}, but DATABASE_URL is {}; one app, one backend",
                    old.dialect().name(),
                    crate::dialect::for_url(&url)?.name()
                )));
            }
            (Some(_), _) => old.dialect(),
            (None, Ok(url)) if old.tables.is_empty() => crate::dialect::for_url(&url)?,
            (None, _) => &crate::dialect::Sqlite,
        };
        let mut new = Snapshot::from_records();
        new.backend = Some(dialect.name().to_owned());
        let changes = crate::migrate::diff_for(&old, &new, dialect);
        if changes.is_empty() {
            println!("No schema changes: the record structs match db/snapshot.json.");
            return Ok(());
        }
        let path = write_migration(&self.migrations_dir(), name, &changes)?;
        let version = path.file_name().and_then(|f| f.to_str()).and_then(|f| f.split('_').next()).unwrap_or_default();
        old.save(&self.snapshot_backup(version))?;
        new.migration = Some(version.to_owned());
        new.save(&self.snapshot_path())?;
        println!("created {}", relative(&path, &self.root));
        for note in &changes.notes {
            println!("  review: {note}");
        }
        println!("next: review the SQL, then `kiln db migrate`");
        Ok(())
    }

    fn secrets(&self, args: &[&str]) -> Result {
        use crate::config::Secrets;
        let open = || {
            Secrets::open(&self.root)?
                .ok_or_else(|| Error::Config("no master key: run `kiln secrets init`, or set KILN_MASTER_KEY".into()))
        };
        match args {
            ["init"] => {
                Secrets::init(&self.root)?;
                println!("config/secrets.enc is ready; config/master.key holds its key (keep it out of git)");
            }
            ["show"] => print!("{}", open()?.to_text()),
            ["set", key, value] => {
                let mut secrets = match Secrets::open(&self.root)? {
                    Some(secrets) => secrets,
                    None => Secrets::init(&self.root)?,
                };
                secrets.values.insert((*key).to_owned(), (*value).to_owned());
                secrets.save()?;
                println!("set {key}");
            }
            ["unset", key] => {
                let mut secrets = open()?;
                secrets.values.remove(*key);
                secrets.save()?;
                println!("removed {key}");
            }
            ["edit"] => {
                let mut secrets = open()?;
                let scratch = std::env::temp_dir().join(format!("kiln-secrets-{}.env", std::process::id()));
                std::fs::write(&scratch, secrets.to_text())?;
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                let status = std::process::Command::new(editor).arg(&scratch).status()?;
                let text = std::fs::read_to_string(&scratch)?;
                let _ = std::fs::remove_file(&scratch);
                if !status.success() {
                    return Err(Error::Config("the editor exited with an error; secrets unchanged".into()));
                }
                secrets.replace_from_text(&text);
                secrets.save()?;
                println!("saved config/secrets.enc");
            }
            _ => return Err(Error::BadRequest("usage: secrets init | show | set KEY VALUE | unset KEY | edit".into())),
        }
        Ok(())
    }

    async fn undo(&self) -> Result {
        let snapshot = Snapshot::load(&self.snapshot_path())?;
        let Some(version) = snapshot.migration.clone() else {
            return Err(Error::BadRequest(
                "nothing to undo: db/snapshot.json doesn't name the migration it was written with".into(),
            ));
        };
        let backup = self.snapshot_backup(&version);
        if !backup.exists() {
            return Err(Error::BadRequest(format!(
                "no saved snapshot from before {version} (it was generated elsewhere); restore db/snapshot.json from version control instead"
            )));
        }
        let migrations = crate::migrate::load_migrations(&self.migrations_dir())?;
        let file = migrations.iter().find(|m| m.version == version);
        if file.is_some_and(|m| m.is_squash()) {
            return Err(Error::BadRequest(format!("{version} is a squash; undo only removes generated migrations")));
        }
        let applied = match self.db().await {
            Ok(db) => db.applied_migrations().await.unwrap_or_default().contains(&version),
            Err(_) => false,
        };
        if let Some(m) = file {
            std::fs::remove_file(&m.path)?;
            println!("removed {}", relative(&m.path, &self.root));
        }
        std::fs::rename(&backup, self.snapshot_path())?;
        println!("restored db/snapshot.json from before {version}");
        if applied {
            println!(
                "the development database has it applied: `kiln db reset` rebuilds it from the remaining migrations"
            );
        }
        Ok(())
    }

    async fn squash(&self, before_year: &str) -> Result {
        if before_year.len() != 4 || !before_year.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::BadRequest(format!("--before takes a year, e.g. 2026 (got `{before_year}`)")));
        }
        // Only squash what has run here: a pending migration may still change.
        let dir = self.migrations_dir();
        let pending = self.db().await?.pending_migrations(&dir).await?;
        if let Some(m) = pending.iter().find(|m| m.version[..4] < *before_year) {
            return Err(Error::BadRequest(format!(
                "{}_{} hasn't been applied here; run `kiln db migrate` before squashing",
                m.version, m.name
            )));
        }
        let squashed = crate::migrate::squash(&dir, before_year)?;
        if squashed.is_empty() {
            println!("Nothing to squash before {before_year}.");
        }
        for s in &squashed {
            println!("{}: {} migrations -> {}", s.year, s.parts, relative(&s.path, &self.root));
            for path in &s.replaced {
                println!("  removed {}", relative(path, &self.root));
            }
        }
        if !squashed.is_empty() {
            println!("databases that ran the originals skip these; commit the change in one go");
        }
        Ok(())
    }

    async fn status(&self, json: bool) -> Result {
        let stale = self.check_snapshot_current(&Snapshot::load(&self.snapshot_path())?).err().map(|e| e.to_string());
        if let (Some(stale), false) = (&stale, json) {
            return Err(Error::BadRequest(stale.clone()));
        }
        let drift = !diff(&Snapshot::load(&self.snapshot_path())?, &Snapshot::from_records()).is_empty();
        let pending = self.db().await?.pending_migrations(&self.migrations_dir()).await?;
        if json {
            let pending: Vec<String> = pending.iter().map(|m| format!("{}_{}", m.version, m.name)).collect();
            println!("{}", serde_json::json!({ "drift": drift, "pending": pending, "stale_snapshot": stale }));
            return Ok(());
        }
        if drift {
            println!("Schema drift: the record structs changed since the last migration. Run `kiln db diff <name>`.");
        }
        if pending.is_empty() {
            println!("No pending migrations.");
        }
        for m in &pending {
            println!("pending {}_{}", m.version, m.name);
        }
        Ok(())
    }

    async fn run_seed(self) -> Result {
        let db = self.db().await?;
        match self.seed {
            Some(seed) => {
                seed(db).await?;
                println!("Seeded.");
                Ok(())
            }
            None => Err(Error::BadRequest("this app has no seed function; pass one to `Cli::seed`".into())),
        }
    }

    async fn reset(self) -> Result {
        if std::env::var("DATABASE_URL").is_ok() {
            return Err(Error::BadRequest("`db reset` only resets the default development database".into()));
        }
        if self.backend().name() == "postgres" {
            self.db().await?.execute_script("DROP SCHEMA public CASCADE; CREATE SCHEMA public;").await?;
        } else {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.sqlite_path().display()));
            }
        }
        let applied = self.db().await?.migrate(&self.migrations_dir()).await?;
        println!("Recreated the development database ({} migrations).", applied.len());
        self.run_seed().await
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

#[cfg(debug_assertions)]
fn print_json(value: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(value).expect("JSON serializes"));
}

#[cfg(debug_assertions)]
fn refuse_in_production(command: &str) -> Result {
    if crate::config::is_production() {
        return Err(Error::BadRequest(format!("`{command}` is a development tool; it doesn't run in production")));
    }
    Ok(())
}

fn manifest() -> serde_json::Value {
    let routes: Vec<_> = routes()
        .iter()
        .map(|r| {
            serde_json::json!({
                "method": r.method, "path": r.path, "action": r.name(), "file": r.file, "line": r.line,
            })
        })
        .collect();
    let records: Vec<_> = records().iter().map(|r| serde_json::json!({ "name": r.name, "table": r.table })).collect();
    serde_json::json!({ "routes": routes, "records": records })
}

/// Job workers per process: `WORKERS`, default 2.
fn workers() -> usize {
    std::env::var("WORKERS").ok().and_then(|w| w.parse().ok()).unwrap_or(2)
}

fn parse_id(id: &str) -> Result<i64> {
    id.trim_start_matches('#').parse().map_err(|_| Error::BadRequest(format!("`{id}` isn't an id")))
}

/// The app's package name from Cargo.toml (`-` as `_`), for default database names.
pub(crate) fn app_name(root: &Path) -> String {
    if let Some(package) = crate::files::package() {
        return package.replace('-', "_");
    }
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package
            && let Some(value) = line.strip_prefix("name")
            && let Some(name) = value.trim_start().strip_prefix('=')
        {
            return name.trim().trim_matches('"').replace('-', "_");
        }
    }
    root.file_name().map(|n| n.to_string_lossy().replace('-', "_")).unwrap_or_else(|| "app".into())
}

/// `<app> health`: asks the server on `PORT` for `/up`, for container health checks (no curl
/// needed in the image).
fn health() -> Result {
    use std::io::{Read, Write};
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let mut stream = std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().map_err(|_| Error::Config(format!("PORT `{port}` isn't a port")))?,
        std::time::Duration::from_secs(3),
    )?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.write_all(b"GET /up HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut response = String::new();
    let _ = stream.take(256).read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).unwrap_or("none");
    if status == "200" {
        println!("up");
        Ok(())
    } else {
        Err(Error::Config(format!("GET /up answered {status}")))
    }
}
