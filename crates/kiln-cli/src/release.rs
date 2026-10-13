//! `kiln build`: the app ready to run somewhere else.
//!
//! - `kiln build` compiles the release binary into `dist/<app>`. Templates, assets,
//!   migrations and secrets are inside it (`kiln::embedded_files!()`), so that one file is
//!   the deploy: run it from a directory where it may write `storage/`.
//! - `kiln build --docker` builds the app's `Dockerfile` (the same as `docker build .`).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::app::App;

pub const DOCKERFILE: &str = include_str!("../templates/deploy/Dockerfile");
pub const ENTRYPOINT: &str = include_str!("../templates/deploy/docker-entrypoint");
pub const DOCKERIGNORE: &str = include_str!("../templates/deploy/dockerignore");

/// Writes the Dockerfile, its entrypoint and `.dockerignore` for `app` (the package name)
/// into `dir`, keeping files that are already there unless `force`. The paths written.
pub fn write_deploy_files(dir: &Path, app: &str, force: bool) -> Result<Vec<PathBuf>, String> {
    let mut written = vec![];
    for (path, contents, executable) in [
        ("Dockerfile", DOCKERFILE, false),
        (".dockerignore", DOCKERIGNORE, false),
        ("bin/docker-entrypoint", ENTRYPOINT, true),
    ] {
        let target = dir.join(path);
        if target.exists() && !force {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&target, contents.replace("@@app@@", app)).map_err(|e| format!("{}: {e}", target.display()))?;
        if executable {
            set_executable(&target)?;
        }
        written.push(target);
    }
    Ok(written)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// `kiln build [--docker]`.
pub fn run(app: &App, docker: bool) -> Result<bool, String> {
    if docker { build_docker(app) } else { build_dist(app) }
}

fn build_dist(app: &App) -> Result<bool, String> {
    println!("building {} (release)", app.package);
    let binary = release_binary(app)?;
    let dist = app.path("dist");
    if dist.exists() {
        fs::remove_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
    }
    fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    let target = dist.join(&app.package);
    fs::copy(&binary, &target).map_err(|e| format!("copying {}: {e}", binary.display()))?;
    let size = fs::metadata(&target).map(|m| m.len() / (1024 * 1024)).unwrap_or(0);
    println!(
        "\nbuilt dist/{app} ({size} MB, with templates, assets and migrations inside). Copy it to a\n\
         machine with the same OS and architecture and run it where it may create `storage/`:\n\n  \
         KILN_ENV=production KILN_SECRET_KEY=<64+ random characters> ./{app} db migrate\n  \
         KILN_ENV=production KILN_SECRET_KEY=... ./{app} serve     # PORT (3000), HOST (0.0.0.0)\n\n\
         For a Linux server from another OS, or a container, use `kiln build --docker`.",
        app = app.package
    );
    Ok(true)
}

/// Compiles the release binary; its path.
fn release_binary(app: &App) -> Result<PathBuf, String> {
    let output = Command::new("cargo")
        .args(["build", "--release", "--message-format=json-render-diagnostics", "--manifest-path"])
        .arg(app.path("Cargo.toml"))
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    if !output.status.success() {
        return Err("the release build failed; see the errors above".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact" && message["target"]["name"] == app.package.as_str())
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| format!("cargo built no `{}` binary", app.package))
}

fn build_docker(app: &App) -> Result<bool, String> {
    if !app.path("Dockerfile").exists() {
        let written = write_deploy_files(&app.root, &app.package, false)?;
        for path in written {
            println!("create  {}", path.strip_prefix(&app.root).unwrap_or(&path).display());
        }
    }
    if !app.path("Cargo.lock").exists() {
        return Err("no Cargo.lock yet; build the app once (`cargo build`) so the image uses the same versions".into());
    }
    let manifest = fs::read_to_string(app.path("Cargo.toml")).map_err(|e| e.to_string())?;
    // The image builds from the app's directory alone, so Kiln has to come from crates.io.
    if let Some(kiln_path) = kiln_path_dependency(&manifest) {
        return Err(format!(
            "the app depends on Kiln by path ({}), which isn't inside the Docker build context; \
             use the published crate (`kiln-framework = \"{}\"` in Cargo.toml) to build the image",
            kiln_path.display(),
            env!("CARGO_PKG_VERSION")
        ));
    }
    let context = app.root.clone();
    let tag = app.package.clone();
    let status = Command::new("docker")
        .args(["build", "--tag", &tag])
        .arg(&context)
        .status()
        .map_err(|e| format!("couldn't run docker ({e}); is Docker installed and running?"))?;
    if !status.success() {
        return Err("docker build failed; see above (is the Docker daemon running?)".into());
    }
    println!(
        "\nbuilt image `{tag}`. Try it:\n\n  docker run --rm -p 8080:8080 -e KILN_SECRET_KEY=$(openssl rand -hex 32) -v {tag}-storage:/app/storage {tag}\n\nthen open http://localhost:8080 (health: /up)."
    );
    Ok(true)
}

/// `kiln = { path = "…" }` from an app's manifest.
fn kiln_path_dependency(manifest: &str) -> Option<PathBuf> {
    let line = manifest.lines().find(|l| {
        let l = l.trim_start();
        ["kiln ", "kiln=", "kiln-framework ", "kiln-framework="].iter().any(|p| l.starts_with(p))
    })?;
    let rest = line.split("path").nth(1)?;
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(PathBuf::from(&rest[start..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_kiln_path() {
        let manifest = "[dependencies]\nkiln-framework = { path = \"/src/kiln/crates/kiln\" }\nserde = \"1\"\n";
        assert_eq!(kiln_path_dependency(manifest), Some(PathBuf::from("/src/kiln/crates/kiln")));
        assert_eq!(kiln_path_dependency("[dependencies]\nkiln = \"0.1\"\n"), None);
    }
}
