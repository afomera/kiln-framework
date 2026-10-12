//! `kiln`: generators, the schema workflow and convention checks for kiln apps.
//!
//! Commands that need the compiled app (`db`, `routes`) run the app binary, which embeds
//! `kiln::Cli`. Everything else works on the source tree.

mod app;
mod check;
mod edit;
mod generate;
mod names;
mod new;

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
    },
    /// Generate code in the app's conventional shape.
    #[command(visible_alias = "g", subcommand)]
    Generate(generate::Generate),
    /// Schema workflow: `db diff <name>`, `db migrate`, `db status`, `db seed`, `db reset`.
    Db {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the route table.
    Routes {
        #[arg(long)]
        json: bool,
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
    /// Run the development server (http://127.0.0.1:3000 unless --port or PORT says otherwise).
    Dev {
        /// Port to listen on (default 3000, or the PORT environment variable).
        #[arg(long)]
        port: Option<u16>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = (|| -> Result<bool, String> {
        if let Command::New { name, kiln } = &cli.command {
            return new::run(name, kiln.as_deref()).map(|()| true);
        }
        let app = app::App::find(cli.app.as_deref())?;
        match cli.command {
            Command::New { .. } => unreachable!("handled above"),
            Command::Generate(g) => g.run(&app).map(|()| true),
            Command::Db { args } => {
                app.run_binary(&[&["db"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Routes { json } => app.run_binary(if json { &["routes", "--json"] } else { &["routes"] }),
            Command::Check { format } => check::run(&app, format == "json"),
            Command::Test { args } => {
                app.cargo(&[&["test", "--quiet"], &args.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat())
            }
            Command::Dev { port } => {
                let env: Vec<(&str, String)> = port.map(|p| ("PORT", p.to_string())).into_iter().collect();
                app.exec_cargo(&["run", "--quiet", "--", "serve"], &env)
            }
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
