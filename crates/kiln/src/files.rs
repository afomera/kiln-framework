//! The app's own files (templates, assets, `public/`, migrations, the schema snapshot and
//! encrypted secrets), from disk or from inside the binary.
//!
//! `kiln::embedded_files!()` in the app's `main.rs` compiles them into release builds, so a
//! deployed app is one binary: no source tree beside it. Debug builds embed nothing and read
//! from disk, so edits show up at once (and live reload works). `KILN_ROOT` makes a release
//! binary read from disk too, from that directory.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// What `kiln::embedded_files!()` produces: the files by path relative to the app's root.
#[derive(Clone, Copy)]
pub struct Embedded {
    pub package: &'static str,
    pub files: &'static [(&'static str, &'static [u8])],
}

impl Embedded {
    /// No files: read everything from disk.
    pub const fn none() -> Self {
        Self { package: "", files: &[] }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

static EMBEDDED: OnceLock<BTreeMap<&'static str, &'static [u8]>> = OnceLock::new();
static PACKAGE: OnceLock<&'static str> = OnceLock::new();

/// Uses `embedded` for the app's files from now on (when it has any).
pub(crate) fn install(embedded: Embedded) {
    if embedded.is_empty() {
        return;
    }
    let _ = PACKAGE.set(embedded.package);
    let _ = EMBEDDED.set(embedded.files.iter().copied().collect());
}

/// Whether the app's files come from inside the binary.
pub(crate) fn embedded() -> bool {
    EMBEDDED.get().is_some()
}

/// The package name the files were embedded for.
pub(crate) fn package() -> Option<&'static str> {
    PACKAGE.get().copied()
}

/// `path` relative to the app's root, as embedded files are keyed.
fn key(path: &Path) -> Option<String> {
    let root = crate::config::root()?;
    let relative = path.strip_prefix(&root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn lookup(path: &Path) -> Option<&'static [u8]> {
    EMBEDDED.get()?.get(key(path)?.as_str()).copied()
}

/// A file's bytes.
pub(crate) fn read(path: &Path) -> Option<Cow<'static, [u8]>> {
    if embedded() {
        return lookup(path).map(Cow::Borrowed);
    }
    std::fs::read(path).ok().map(Cow::Owned)
}

/// A text file.
pub(crate) fn read_string(path: &Path) -> Option<String> {
    read(path).and_then(|bytes| String::from_utf8(bytes.into_owned()).ok())
}

pub(crate) fn is_file(path: &Path) -> bool {
    if embedded() {
        return lookup(path).is_some();
    }
    path.is_file()
}

pub(crate) fn is_dir(path: &Path) -> bool {
    if embedded() {
        let Some(prefix) = key(path) else { return false };
        let prefix = format!("{}/", prefix.trim_end_matches('/'));
        return EMBEDDED.get().is_some_and(|files| files.keys().any(|k| k.starts_with(&prefix)));
    }
    path.is_dir()
}

/// Every file under `dir`, at any depth.
pub(crate) fn walk(dir: &Path) -> Vec<PathBuf> {
    if embedded() {
        let (Some(root), Some(prefix)) = (crate::config::root(), key(dir)) else { return vec![] };
        let prefix = format!("{}/", prefix.trim_end_matches('/'));
        return EMBEDDED
            .get()
            .map(|files| files.keys().filter(|k| k.starts_with(&prefix)).map(|k| root.join(k)).collect())
            .unwrap_or_default();
    }
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// Templates by name (`posts/show.html.jinja`), for the template loader.
pub(crate) fn template(name: &str) -> Option<String> {
    let files = EMBEDDED.get()?;
    files.get(format!("templates/{name}").as_str()).and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
}
