use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// A kiln app on disk: a crate with `src/actions/`.
pub struct App {
    pub root: PathBuf,
    /// Package name from Cargo.toml.
    pub package: String,
}

impl App {
    /// `explicit`, or the nearest directory at or above the current one that is a kiln app.
    pub fn find(explicit: Option<&Path>) -> Result<Self, String> {
        let start = match explicit {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let mut dir = Some(start.as_path());
        while let Some(candidate) = dir {
            if let Some(app) = Self::at(candidate) {
                return Ok(app);
            }
            if explicit.is_some() {
                break;
            }
            dir = candidate.parent();
        }
        Err(format!(
            "no kiln app found at or above {}; run inside an app (a crate with src/actions/) or pass --app <dir>",
            start.display()
        ))
    }

    fn at(dir: &Path) -> Option<Self> {
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        if !dir.join("src/actions").is_dir() {
            return None;
        }
        let package = package_name(&manifest)?;
        Some(Self { root: dir.to_path_buf(), package })
    }

    /// The crate name used in paths, e.g. `my-app` → `my_app`.
    pub fn crate_name(&self) -> String {
        self.package.replace('-', "_")
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Runs `cargo <args>` for the app's manifest. Returns whether it succeeded.
    pub fn cargo(&self, args: &[&str]) -> Result<bool, String> {
        self.cargo_with_env(args, &[])
    }

    /// Like [`App::cargo`], with extra environment variables for the child process.
    pub fn cargo_with_env(&self, args: &[&str], env: &[(&str, String)]) -> Result<bool, String> {
        let manifest = self.path("Cargo.toml");
        let (first, rest) = args.split_first().ok_or("no cargo command")?;
        let status = Command::new("cargo")
            .arg(first)
            .arg("--manifest-path")
            .arg(&manifest)
            .args(rest)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .status()
            .map_err(|e| format!("couldn't run cargo: {e}"))?;
        Ok(status.success())
    }

    /// Runs `cargo <args>` in place of this process (on Unix), so signals sent to `kiln dev`
    /// reach the server instead of leaving it running. Cargo execs the app binary in turn.
    pub fn exec_cargo(&self, args: &[&str], env: &[(&str, String)]) -> Result<bool, String> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let (first, rest) = args.split_first().ok_or("no cargo command")?;
            let error = Command::new("cargo")
                .arg(first)
                .arg("--manifest-path")
                .arg(self.path("Cargo.toml"))
                .args(rest)
                .envs(env.iter().map(|(k, v)| (k, v)))
                .exec();
            Err(format!("couldn't run cargo: {error}"))
        }
        #[cfg(not(unix))]
        self.cargo_with_env(args, env)
    }

    /// Runs the app binary with `args` (e.g. `db status`), streaming its output.
    pub fn run_binary(&self, args: &[&str]) -> Result<bool, String> {
        let mut full = vec!["run", "--quiet", "--"];
        full.extend_from_slice(args);
        self.cargo(&full)
    }

    /// Runs the app binary with `args` and returns its stdout, or `None` if it failed.
    pub fn binary_output(&self, args: &[&str]) -> Result<Option<String>, String> {
        let output = Command::new("cargo")
            .args(["run", "--quiet", "--manifest-path"])
            .arg(self.path("Cargo.toml"))
            .arg("--")
            .args(args)
            .output()
            .map_err(|e| format!("couldn't run cargo: {e}"))?;
        Ok(output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
    }
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(value) = line.strip_prefix("name") {
            let value = value.trim_start().strip_prefix('=')?.trim().trim_matches('"');
            return Some(value.to_owned());
        }
    }
    None
}
