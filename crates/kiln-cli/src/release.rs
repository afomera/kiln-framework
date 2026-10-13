//! `kiln build`: the app ready to run somewhere else.
//!
//! - `kiln build` compiles the release binary into `dist/<app>`. Templates, assets,
//!   migrations and secrets are inside it (`kiln::embedded_files!()`), so that one file is
//!   the deploy: run it from a directory where it may write `storage/`.
//! - `kiln build --docker` prepares a Docker build context in `target/kiln-docker/` (the app,
//!   plus Kiln's own crates while Kiln isn't on crates.io) and builds the app's `Dockerfile`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::app::App;

pub const DOCKERFILE: &str = include_str!("../templates/deploy/Dockerfile");
pub const ENTRYPOINT: &str = include_str!("../templates/deploy/docker-entrypoint");
pub const DOCKERIGNORE: &str = include_str!("../templates/deploy/dockerignore");

/// Never copied into a build context: build output, local data and the master key.
const EXCLUDED: &[&str] =
    &["target", "dist", "storage", "tmp", ".git", "db/.kiln", "config/master.key", "node_modules"];

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
    let context = app.path("target/kiln-docker");
    if context.exists() {
        fs::remove_dir_all(&context).map_err(|e| format!("{}: {e}", context.display()))?;
    }
    copy_tree(&app.root, &context.join("app"), EXCLUDED)?;
    // Kiln itself, until it's on crates.io: the app's `kiln = { path = .. }` points into a
    // workspace, so copy that workspace's kiln crates and point the app at the copy.
    let manifest_path = context.join("app/Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    if let Some(kiln_path) = kiln_path_dependency(&manifest) {
        let kiln_crate = if kiln_path.is_absolute() { kiln_path.clone() } else { app.root.join(&kiln_path) };
        let workspace = kiln_crate
            .parent()
            .and_then(Path::parent)
            .filter(|w| w.join("Cargo.toml").exists())
            .ok_or_else(|| format!("can't find the Kiln workspace around {}", kiln_crate.display()))?;
        for krate in ["crates/kiln", "crates/kiln-macros"] {
            copy_tree(&workspace.join(krate), &context.join("kiln").join(krate), &["target"])?;
        }
        let root = fs::read_to_string(workspace.join("Cargo.toml")).map_err(|e| e.to_string())?;
        fs::write(context.join("kiln/Cargo.toml"), kiln_only_workspace(&root)).map_err(|e| e.to_string())?;
        let original = format!("\"{}\"", kiln_path.display());
        fs::write(&manifest_path, manifest.replacen(&original, "\"../kiln/crates/kiln\"", 1))
            .map_err(|e| e.to_string())?;
    }
    fs::copy(app.path("Dockerfile"), context.join("Dockerfile")).map_err(|e| e.to_string())?;
    println!("build context: {}\n", context.strip_prefix(&app.root).unwrap_or(&context).display());
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
    let line = manifest.lines().find(|l| l.trim_start().starts_with("kiln ") || l.trim_start().starts_with("kiln="))?;
    let rest = line.split("path").nth(1)?;
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(PathBuf::from(&rest[start..end]))
}

/// The Kiln workspace manifest with only the crates an app builds against.
fn kiln_only_workspace(root: &str) -> String {
    root.lines()
        .map(|line| {
            if line.trim_start().starts_with("members") {
                "members = [\"crates/kiln\", \"crates/kiln-macros\"]".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copies `from` (a file or directory) to `to`, skipping `excluded` paths (relative to
/// `from`) and dotfiles other than `.dockerignore`.
fn copy_tree(from: &Path, to: &Path, excluded: &[&str]) -> Result<(), String> {
    fn walk(base: &Path, dir: &Path, to: &Path, excluded: &[&str]) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let relative = path.strip_prefix(base).unwrap_or(&path);
            let rel = relative.to_string_lossy().replace('\\', "/");
            let name = entry.file_name().to_string_lossy().into_owned();
            let skip = excluded.iter().any(|x| rel == *x || rel.starts_with(&format!("{x}/")))
                || (rel.starts_with("db/") && rel.contains(".sqlite3"))
                || (name.starts_with('.') && name != ".dockerignore");
            if skip {
                continue;
            }
            let target = to.join(relative);
            if path.is_dir() {
                fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                walk(base, &path, to, excluded)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(&path, &target).map_err(|e| format!("copying {}: {e}", path.display()))?;
            }
        }
        Ok(())
    }
    if from.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(from, to).map_err(|e| format!("copying {}: {e}", from.display()))?;
        return Ok(());
    }
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    walk(from, from, to, excluded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_kiln_path() {
        let manifest = "[dependencies]\nkiln = { path = \"/src/kiln/crates/kiln\" }\nserde = \"1\"\n";
        assert_eq!(kiln_path_dependency(manifest), Some(PathBuf::from("/src/kiln/crates/kiln")));
        assert_eq!(kiln_path_dependency("[dependencies]\nkiln = \"0.1\"\n"), None);
    }

    #[test]
    fn the_workspace_keeps_only_kilns_crates() {
        let root = "[workspace]\nmembers = [\"crates/kiln\", \"crates/kiln-cli\", \"examples/blog\"]\n\n[workspace.dependencies]\nserde = \"1\"\n";
        let trimmed = kiln_only_workspace(root);
        assert!(trimmed.contains("members = [\"crates/kiln\", \"crates/kiln-macros\"]"));
        assert!(trimmed.contains("serde = \"1\""));
        assert!(!trimmed.contains("examples/blog"));
    }
}
