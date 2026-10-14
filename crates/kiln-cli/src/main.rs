//! `kiln`: generators, the schema workflow and convention checks for kiln apps.
//!
//! Commands that need the compiled app (`db`, `routes`) run the app binary, which embeds
//! `kiln::Cli`. Everything else works on the source tree.

mod app;
mod check;
mod dev;
mod edit;
mod explain;
mod generate;
mod introspect;
mod mcp;
mod names;
mod new;
mod release;
mod template_check;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kiln", version, about = "Generators, schema workflow and checks for kiln apps")]
struct Cli {
    /// The app's directory (the crate with `src/actions/`). Defaults to the nearest one.
    #[arg(long, global = true)]
    app: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new app: `kiln new journal`.
    New {
        name: String,
        /// Path to the kiln crate (defaults to the one this CLI was built from).
        #[arg(long)]
        kiln: Option<std::path::PathBuf>,
        /// The database backend: `sqlite` (default) or `postgres`.
        #[arg(long, default_value = "sqlite")]
        database: String,
    },
    /// Generate code in the app's conventional shape.
    #[command(visible_alias = "g", subcommand)]
    Generate(generate::Generate),
    /// Schema workflow: `db diff <name>`, `db migrate`, `db status`, `db seed`, `db reset`.
    Db {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Encrypted secrets: `secrets init | show | set KEY VALUE | unset KEY | edit`.
    Secrets {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Background jobs: `jobs` (counts, queues, failures), `jobs retry [id]`, `jobs cancel <id>`,
    /// `jobs run <id>`, `jobs discard-failed`, `jobs pause|resume <queue>`, `jobs work`
    /// (a worker without the web server).
    Jobs {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Recurring jobs: `schedules`, `schedules pause|resume|run <job name>`.
    Schedules {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Job batches: `batches`, `batches cancel <id>`.
    Batches {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The whole app at a glance: routes, records, views, forms, policies, ops, jobs, mail,
    /// config and templates, with file:line. `--json` for agents.
    Introspect {
        /// Only this part: routes, records, views, forms, policies, ops, jobs, mail, ...
        section: Option<String>,
        /// Only items mentioning this text (a record, a path, a file): `--find Event`.
        #[arg(long)]
        find: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The long form of a `kiln check` code or an error page's code: `kiln explain K0301`.
    /// Without a code, lists them all (and the app's own rules).
    Explain { code: Option<String> },
    /// An MCP server for agents, over stdio: `claude mcp add kiln -- kiln mcp`. Tools:
    /// introspect, request (as any user, with SQL, jobs and mail), query (read-only SQL),
    /// mail, jobs, check, explain, test.
    Mcp {
        /// Allow requests other than GET and HEAD (they change the development database).
        #[arg(long)]
        write: bool,
    },
    /// Print the route table.
    Routes {
        #[arg(long)]
        json: bool,
    },
    /// A release build to deploy: `dist/` (binary plus templates, assets, migrations), or
    /// with `--docker` the app's production image (see its Dockerfile).
    Build {
        #[arg(long)]
        docker: bool,
    },
    /// Compile, then check conventions, schema drift and template routes.
    Check {
        /// `text` (default) or `json`.
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Run the app's tests (`cargo test` for the app crate).
    Test {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the app for development: builds it, runs `Procfile.dev` (default `web: $APP
    /// serve`), and rebuilds and restarts on changes to src/. Port 3000 unless --port or PORT.
    /// In a terminal it shows a TUI (keys in its footer; q quits); piped or in CI it
    /// prints prefixed lines.
    Dev {
        /// Port to listen on (default 3000, or the PORT environment variable).
        #[arg(long)]
        port: Option<u16>,
        /// Print prefixed lines (`web | ...`) instead of the TUI.
        #[arg(long, conflicts_with = "json")]
        inline: bool,
        /// Print NDJSON events (start, output, exit, restarting, failed, build) instead of the TUI.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = (|| -> Result<bool, String> {
        if let Command::New { name, kiln, database } = &cli.command {
            return new::run(name, kiln.as_deref(), database).map(|()| true);
        }
        if let Command::Explain { code } = &cli.command {
            return explain::run(app::App::find(cli.app.as_deref()), code.as_deref());
        }
        let app = app::App::find(cli.app.as_deref())?;
        match cli.command {
            Command::New { .. } | Command::Explain { .. } => unreachable!("handled above"),
            Command::Generate(g) => g.run(&app).map(|()| true),
            Command::Db { args } => {
                app.run_binary(&[&["db"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Secrets { args } => {
                app.run_binary(&[&["secrets"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Jobs { args } => match args.first().map(String::as_str) {
                Some("work") => app.exec_cargo(&["run", "--quiet", "--", "work"], &[]),
                _ => app.run_binary(&[&["jobs"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat()),
            },
            Command::Schedules { args } => {
                app.run_binary(&[&["schedules"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Batches { args } => {
                app.run_binary(&[&["batches"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Routes { json } => app.run_binary(if json { &["routes", "--json"] } else { &["routes"] }),
            Command::Introspect { section, find, json } => {
                introspect::run(&app, section.as_deref(), find.as_deref(), json)
            }
            Command::Mcp { write } => mcp::run(&app, write),
            Command::Check { format } => check::run(&app, format == "json"),
            Command::Build { docker } => release::run(&app, docker),
            Command::Test { args } => {
                app.cargo(&[&["test", "--quiet"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Dev { port, inline, json } => dev::run(&app, dev::Options { port, inline, json }),
        }
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
