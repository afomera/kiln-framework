//! Static files.
//!
//! - `assets/`: the app's CSS, JS and images, served at content-fingerprinted URLs
//!   (`{{ asset("app.css") }}` → `/assets/app-1a2b3c4d.css`) and cached forever.
//! - `public/`: served as-is at the site root (`robots.txt`, `favicon.ico`) when no route
//!   matches.
//! - Kiln's own JavaScript (Turbo) at `/kiln/turbo-<version>-<hash>.js`, and the rich text
//!   editor (Lexxy plus Kiln's setup and skin) at `/kiln/lexxy-*`, `/kiln/rich-text-*`.
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

/// Lexxy, vendored by `scripts/vendor-lexxy.sh` (see `vendor/README.md`), byte for byte.
const LEXXY_JS: &str = include_str!("../vendor/lexxy/lexxy.min.js");
const LEXXY_VERSION: &str = include_str!("../vendor/lexxy/VERSION");
const LEXXY_LICENSE: &str = include_str!("../vendor/lexxy/MIT-LICENSE");
const LEXXY_NOTICES: &str = include_str!("../vendor/lexxy/THIRD-PARTY-NOTICES.md");
const LEXXY_CSS: [&str; 3] = [
    include_str!("../vendor/lexxy/lexxy-variables.css"),
    include_str!("../vendor/lexxy/lexxy-content.css"),
    include_str!("../vendor/lexxy/lexxy-editor.css"),
];
/// Kiln's own editor code: configuration and skin.
const RICH_TEXT_JS: &str = include_str!("../assets/rich-text.js");
const RICH_TEXT_CSS: &str = include_str!("../assets/rich-text.css");
const RICH_TEXT_DISPLAY_JS: &str = include_str!("../assets/rich-text-display.js");
const CONFIRM_JS: &str = include_str!("../assets/confirm.js");
const DIRECT_UPLOAD_JS: &str = include_str!("../assets/direct-upload.js");
const DEV_RELOAD_JS: &str = include_str!("../assets/dev-reload.js");

/// A file Kiln serves at `/kiln/<name>-<hash>.<ext>`, built once.
struct Served {
    url: String,
    body: Vec<u8>,
}

impl Served {
    fn new(name: &str, ext: &str, body: String) -> Self {
        let url = format!("/kiln/{name}-{}.{ext}", short_hash(body.as_bytes()));
        Self { url, body: body.into_bytes() }
    }
}

/// The minified build carries no notices, so the served copy starts with all of them:
/// Lexxy's MIT license and those of the libraries it bundles.
fn lexxy_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| {
        let version = LEXXY_VERSION.trim();
        let header = format!(
            "/*! Lexxy {version} | https://github.com/basecamp/lexxy | vendored and served by Kiln, unmodified below this comment\n\n{}\n\n{}\n*/\n",
            LEXXY_LICENSE.trim(),
            LEXXY_NOTICES.trim()
        );
        Served::new(&format!("lexxy-{version}"), "js", format!("{header}{LEXXY_JS}"))
    })
}

fn rich_text_display_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("rich-text-display", "js", RICH_TEXT_DISPLAY_JS.to_owned()))
}

fn direct_upload_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("direct-upload", "js", DIRECT_UPLOAD_JS.to_owned()))
}

fn confirm_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("confirm", "js", CONFIRM_JS.to_owned()))
}

/// The URL of Kiln's confirm dialog module (loaded on every page with Turbo).
pub(crate) fn confirm_url() -> &'static str {
    &confirm_js().url
}

fn dev_reload_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("dev-reload", "js", DEV_RELOAD_JS.to_owned()))
}

fn rich_text_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("rich-text", "js", RICH_TEXT_JS.to_owned()))
}

/// Lexxy's three stylesheets (their `@import`s dropped: they're inlined here) and Kiln's skin.
fn rich_text_css() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| {
        let mut css =
            format!("/*! Lexxy {} stylesheets | MIT License, Copyright Jorge Manrubia */\n", LEXXY_VERSION.trim());
        for sheet in LEXXY_CSS {
            css.extend(sheet.lines().filter(|l| !l.trim_start().starts_with("@import")).flat_map(|l| [l, "\n"]));
        }
        css.push_str("\n/* Kiln's skin */\n");
        css.push_str(RICH_TEXT_CSS);
        Served::new("rich-text", "css", css)
    })
}

/// The stylesheet `RichText` fields and `| rich_text` output need.
pub(crate) fn rich_text_css_url() -> &'static str {
    &rich_text_css().url
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

    /// The app's `assets/` directory.
    pub(crate) fn dir(&self) -> &Path {
        &self.assets
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
        let mut imports = BTreeMap::from([
            ("@hotwired/turbo".to_owned(), turbo_url().to_owned()),
            // Loaded only by pages with an editor (`{{ field(..) }}` on a `RichText`).
            ("lexxy".to_owned(), lexxy_js().url.clone()),
            // Lexxy uploads attachments through Active Storage's `DirectUpload`; Kiln's own.
            ("@rails/activestorage".to_owned(), direct_upload_js().url.clone()),
            ("kiln/direct-upload".to_owned(), direct_upload_js().url.clone()),
            ("kiln/confirm".to_owned(), confirm_js().url.clone()),
            ("kiln/dev-reload".to_owned(), dev_reload_js().url.clone()),
            ("kiln/rich-text".to_owned(), rich_text_js().url.clone()),
            ("kiln/rich-text-display".to_owned(), rich_text_display_js().url.clone()),
        ]);
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
        // `/` is `public/index.html` when there is one.
        let mut path = if name.is_empty() { self.public.clone() } else { self.public.join(safe_relative(name)?) };
        if path.is_dir() {
            path = path.join("index.html");
        }
        let bytes = std::fs::read(&path).ok()?;
        Some(file_response(&path.to_string_lossy(), bytes, false))
    }
}

/// Serves kiln's own files under `/kiln/`.
pub(crate) fn serve_kiln(request_path: &str) -> Option<Response> {
    if request_path == turbo_url() {
        return Some(file_response("turbo.js", TURBO_JS.to_vec(), true));
    }
    [
        (lexxy_js(), "lexxy.js"),
        (rich_text_js(), "rich-text.js"),
        (rich_text_display_js(), "rich-text-display.js"),
        (confirm_js(), "confirm.js"),
        (direct_upload_js(), "direct-upload.js"),
        (dev_reload_js(), "dev-reload.js"),
        (rich_text_css(), "rich-text.css"),
    ]
    .into_iter()
    .find(|(served, _)| served.url == request_path)
    .map(|(served, name)| file_response(name, served.body.clone(), true))
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
