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
  jobs                Job counts and recent failures
  jobs retry [id]     Queue failed jobs (one, or all) to run again
  db migrate          Apply pending migrations
  db diff <name>      Write a migration for changes to the record structs
  db status [--json]  Pending migrations and schema drift
  db seed             Run the app's seed function
  db reset            Recreate the development database, migrate and seed
  secrets ...         Encrypted secrets: init | show | set KEY VALUE | unset KEY | edit
  routes [--json]     The route table
  manifest            Routes and records as JSON
";

impl Cli {
    /// `root` is the app crate's directory: pass `env!("CARGO_MANIFEST_DIR")`.
    pub fn new(root: &str) -> Self {
        crate::config::set_root(std::path::Path::new(root));
        Self { root: PathBuf::from(root), seed: None, configure: None }
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

    fn database_url(&self) -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| format!("sqlite://{}", self.root.join("db/development.sqlite3").display()))
    }

    async fn db(&self) -> Result<Db> {
        std::fs::create_dir_all(self.root.join("db"))?;
        Db::connect(&self.database_url()).await
    }

    async fn dispatch(self, args: &[&str]) -> Result {
        match args {
            [] | ["serve"] => self.serve().await,
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
                for (id, name, attempts, error) in db.failed_jobs().await? {
                    println!("\nfailed #{id} {name} after {attempts} attempt(s):\n  {error}");
                }
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
        if workers() > 0 {
            tokio::spawn(crate::jobs::work(app.ctx().clone(), workers()));
        }
        let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
        app.serve(&format!("127.0.0.1:{port}")).await
    }

    /// The configured app, refusing to start with pending migrations.
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
        let new = Snapshot::from_records();
        let changes = diff(&old, &new);
        if changes.is_empty() {
            println!("No schema changes: the record structs match db/snapshot.json.");
            return Ok(());
        }
        let path = write_migration(&self.migrations_dir(), name, &changes)?;
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
            Secrets::open(&self.root)?.ok_or_else(|| {
                Error::Config("no master key: run `kiln secrets init`, or set KILN_MASTER_KEY".into())
            })
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

    async fn status(&self, json: bool) -> Result {
        let drift = !diff(&Snapshot::load(&self.snapshot_path())?, &Snapshot::from_records()).is_empty();
        let pending = self.db().await?.pending_migrations(&self.migrations_dir()).await?;
        if json {
            let pending: Vec<String> = pending.iter().map(|m| format!("{}_{}", m.version, m.name)).collect();
            println!("{}", serde_json::json!({ "drift": drift, "pending": pending }));
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
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(self.root.join(format!("db/development.sqlite3{suffix}")));
        }
        let applied = self.db().await?.migrate(&self.migrations_dir()).await?;
        println!("Recreated the development database ({} migrations).", applied.len());
        self.run_seed().await
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
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
