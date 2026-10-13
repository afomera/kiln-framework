//! Static files.
//!
//! - `assets/`: the app's CSS, JS and images, served at content-fingerprinted URLs
//!   (`{{ asset("app.css") }}` → `/assets/app-1a2b3c4d.css`) and cached forever.
//! - `public/`: served as-is at the site root (`robots.txt`, `favicon.ico`) when no route
//!   matches.
//! - Kiln's own JavaScript (Turbo) at `/kiln/turbo-<version>-<hash>.js`.
//!
//! JavaScript loads through an import map (`{{ javascript_importmap() }}`), so there's no
//! bundler or Node toolchain.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};

/// Turbo, vendored (see `vendor/README.md`).
const TURBO_VERSION: &str = "8.0.12";
const TURBO_JS: &[u8] = include_bytes!("../vendor/turbo-8.0.12.js");

/// The URL Turbo is served at, fingerprinted so upgrades bust caches.
pub(crate) fn turbo_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| format!("/kiln/turbo-{TURBO_VERSION}-{}.js", short_hash(TURBO_JS)))
}

fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// `assets/js/app.js` + hash → `js/app-1a2b3c4d.js`.
fn fingerprinted(name: &str, hash: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.ends_with('/') => format!("{stem}-{hash}.{ext}"),
        _ => format!("{name}-{hash}"),
    }
}

/// `js/app-1a2b3c4d.js` → (`js/app.js`, `1a2b3c4d`).
fn split_fingerprint(name: &str) -> Option<(String, &str)> {
    let (stem, ext) = name.rsplit_once('.').map_or((name, None), |(s, e)| (s, Some(e)));
    let (base, hash) = stem.rsplit_once('-')?;
    if hash.len() != 8 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((ext.map_or_else(|| base.to_owned(), |e| format!("{base}.{e}")), hash))
}

/// A relative path with no `..`, root or prefix components.
fn safe_relative(name: &str) -> Option<&Path> {
    let path = Path::new(name);
    path.components().all(|c| matches!(c, Component::Normal(_))).then_some(path)
}

/// Content hashes of asset files, cached until the file changes.
fn file_hash(path: &Path) -> Option<String> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, (SystemTime, String)>>> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, hash)) = cache.get(path)
        && *at == modified
    {
        return Some(hash.clone());
    }
    let hash = short_hash(&std::fs::read(path).ok()?);
    cache.insert(path.to_owned(), (modified, hash.clone()));
    Some(hash)
}

/// The app's directory layout for static files.
#[derive(Debug, Clone)]
pub(crate) struct Assets {
    assets: PathBuf,
    public: PathBuf,
}

impl Assets {
    /// From the app's root directory (the crate with `templates/`, `assets/`, `public/`).
    pub fn at(root: &Path) -> Self {
        Self { assets: root.join("assets"), public: root.join("public") }
    }

    /// From a `templates/` directory: assets live next to it.
    pub fn beside_templates(templates: &str) -> Self {
        Self::at(Path::new(templates).parent().unwrap_or(Path::new(".")))
    }

    /// `/assets/app-1a2b3c4d.css` for `app.css`, or `None` if there's no such file.
    pub fn url(&self, name: &str) -> Option<String> {
        let relative = safe_relative(name)?;
        let hash = file_hash(&self.assets.join(relative))?;
        Some(format!("/assets/{}", fingerprinted(name, &hash)))
    }

    /// Every JavaScript file under `assets/`, as import-map entries: `application` →
    /// `/assets/application-…js`, `controllers/hello` → `/assets/controllers/hello-…js`.
    pub fn importmap(&self) -> BTreeMap<String, String> {
        let mut imports = BTreeMap::from([("@hotwired/turbo".to_owned(), turbo_url().to_owned())]);
        let mut stack = vec![self.assets.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Ok(relative) = path.strip_prefix(&self.assets) else { continue };
                let name = relative.to_string_lossy().replace('\\', "/");
                if let (Some(module), Some(url)) = (name.strip_suffix(".js"), self.url(&name)) {
                    imports.insert(module.to_owned(), url);
                }
            }
        }
        imports
    }

    pub fn has_application_js(&self) -> bool {
        self.assets.join("application.js").is_file()
    }

    /// Serves `/assets/<fingerprinted name>`.
    pub fn serve_asset(&self, requested: &str) -> Option<Response> {
        let (name, hash) = split_fingerprint(requested)?;
        let path = self.assets.join(safe_relative(&name)?);
        let bytes = std::fs::read(&path).ok()?;
        // A stale fingerprint still gets the current file, just not cached forever.
        let immutable = short_hash(&bytes) == hash;
        Some(file_response(&name, bytes, immutable))
    }

    /// Serves a file from `public/`, if there is one.
    pub fn serve_public(&self, request_path: &str) -> Option<Response> {
        let name = request_path.trim_start_matches('/');
        let mut path = self.public.join(safe_relative(name).filter(|_| !name.is_empty())?);
        if path.is_dir() {
            path = path.join("index.html");
        }
        let bytes = std::fs::read(&path).ok()?;
        Some(file_response(&path.to_string_lossy(), bytes, false))
    }
}

/// Serves kiln's own files under `/kiln/`.
pub(crate) fn serve_kiln(request_path: &str) -> Option<Response> {
    (request_path == turbo_url()).then(|| file_response("turbo.js", TURBO_JS.to_vec(), true))
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "xml" => "application/xml",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn file_response(name: &str, bytes: Vec<u8>, immutable: bool) -> Response {
    let cache = if immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut response = (StatusCode::OK, Body::from(bytes)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(name)));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_round_trip() {
        assert_eq!(fingerprinted("js/app.js", "1a2b3c4d"), "js/app-1a2b3c4d.js");
        assert_eq!(split_fingerprint("js/app-1a2b3c4d.js"), Some(("js/app.js".to_owned(), "1a2b3c4d")));
        assert_eq!(split_fingerprint("my-app-1a2b3c4d.css"), Some(("my-app.css".to_owned(), "1a2b3c4d")));
        assert_eq!(split_fingerprint("app.css"), None);
    }

    #[test]
    fn rejects_traversal() {
        assert!(safe_relative("../secret").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert!(safe_relative("js/app.js").is_some());
    }

    #[test]
    fn turbo_url_is_fingerprinted() {
        assert!(turbo_url().starts_with("/kiln/turbo-8.0.12-"));
        assert!(serve_kiln(turbo_url()).is_some());
    }
}
