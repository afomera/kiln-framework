//! `kiln new <app>`: a starter app that compiles, has its first migration, and passes its
//! tests.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::names::{human, is_snake};

macro_rules! starter {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/new/", $path)))),*]
    };
}

const FILES: &[(&str, &str)] = starter![
    "Cargo.toml",
    "rustfmt.toml",
    "gitignore",
    "src/main.rs",
    "src/lib.rs",
    "src/app.rs",
    "src/seed.rs",
    "src/factories.rs",
    "src/records/mod.rs",
    "src/records/user.rs",
    "src/forms/mod.rs",
    "src/forms/sign_in_form.rs",
    "src/ops/mod.rs",
    "src/policies/mod.rs",
    "src/actions/mod.rs",
    "src/actions/home/mod.rs",
    "src/actions/home/index.rs",
    "src/actions/sessions/mod.rs",
    "src/actions/sessions/new.rs",
    "src/actions/sessions/create.rs",
    "src/actions/sessions/destroy.rs",
    "src/views/mod.rs",
    "src/views/current_user.rs",
    "src/views/home/mod.rs",
    "src/views/home/index.rs",
    "src/views/sessions/mod.rs",
    "src/views/sessions/new.rs",
    "templates/layouts/app.html.jinja",
    "assets/app.css",
    "assets/application.js",
    "public/robots.txt",
    "Procfile.dev",
    "public/favicon.svg",
    "templates/home/index.html.jinja",
    "templates/sessions/new.html.jinja",
    "tests/sessions.rs",
];

pub const AGENTS_MD: &str = include_str!("../templates/AGENTS.md");

/// The kiln crate this CLI was built from, used until kiln is published.
fn default_kiln_path() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kiln");
    path.canonicalize().unwrap_or(path)
}

pub fn run(name: &str, kiln: Option<&Path>) -> Result<(), String> {
    let dir = PathBuf::from(name);
    let app = dir.file_name().and_then(|n| n.to_str()).ok_or("give the app a name, e.g. `kiln new journal`")?;
    let crate_name = app.replace('-', "_");
    if !is_snake(&crate_name) {
        return Err(format!("`{app}` isn't a valid app name; use lowercase letters, digits, `-` or `_`"));
    }
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let kiln_path = match kiln {
        Some(path) => path.canonicalize().map_err(|e| format!("--kiln {}: {e}", path.display()))?,
        None => default_kiln_path(),
    };
    if !kiln_path.join("Cargo.toml").exists() {
        return Err(format!("no kiln crate at {}; pass --kiln <path to crates/kiln>", kiln_path.display()));
    }

    let title = human(&crate_name);
    for (path, contents) in FILES {
        let target = dir.join(if *path == "gitignore" { ".gitignore" } else { path });
        let contents = contents
            .replace("@@app@@", app)
            .replace("@@crate@@", &crate_name)
            .replace("@@App@@", &title)
            .replace("@@kiln_path@@", &kiln_path.display().to_string());
        write(&target, &contents)?;
        println!("create  {}", target.display());
    }
    write(&dir.join("AGENTS.md"), AGENTS_MD)?;
    println!("create  {}", dir.join("AGENTS.md").display());
    let rust_files: Vec<PathBuf> = FILES.iter().filter(|(p, _)| p.ends_with(".rs")).map(|(p, _)| dir.join(p)).collect();
    let _ = Command::new("rustfmt").args(["--edition", "2024"]).args(&rust_files).status();

    // The first migration comes from the User struct, like every later one.
    println!("\ncompiling and writing the first migration (the first build takes a minute)");
    let manifest = dir.join("Cargo.toml");
    let status = Command::new("cargo")
        .args(["run", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .args(["--", "db", "diff", "create_users"])
        .status()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    if !status.success() {
        return Err("the new app didn't build; see the errors above".into());
    }
    let status = Command::new("cargo")
        .args(["run", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .args(["--", "db", "reset"])
        .status()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    if !status.success() {
        return Err("the first migration didn't apply; see the errors above".into());
    }
    println!(
        "\ncreated {app}. Next:\n  cd {}\n  kiln dev        # http://127.0.0.1:3000\n  kiln check\n  kiln test",
        dir.display()
    );
    Ok(())
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}
