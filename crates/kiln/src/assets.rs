//! Static files.
//!
//! - `assets/`: the app's CSS, JS and images, served at content-fingerprinted URLs
//!   (`{{ asset("app.css") }}` → `/assets/app-1a2b3c4d.css`) and cached forever.
//!   Stylesheets are served with their `url(..)` and `@import ".."` references to other
//!   assets rewritten to fingerprinted URLs: relative to the stylesheet
//!   (`url(../fonts/a.woff2)` in `css/app.css` is `assets/fonts/a.woff2`) or under
//!   `/assets/` (`url(/assets/fonts/a.woff2)`). A stylesheet's fingerprint is of the
//!   rewritten CSS, so it changes when a font or image it uses does. Other URLs (`https:`,
//!   `//cdn`, `data:`, `#id`, `/` paths outside `assets/` such as `public/` files) and
//!   references to missing files (logged once) are left as written; `?query` and `#fragment`
//!   are kept.
//! - `public/`: served as-is at the site root (`robots.txt`, `favicon.ico`), ahead of routes
//!   that merely match the path (`/{owner}`); a route for exactly that path wins. A
//!   directory's `index.html` is served only when no route matches.
//! - Kiln's own JavaScript (Turbo, Stimulus) at `/kiln/turbo-<version>-<hash>.js` and
//!   `/kiln/stimulus-*`, and the rich text editor (Lexxy plus Kiln's setup and skin) at
//!   `/kiln/lexxy-*`, `/kiln/rich-text-*`.
//!
//! JavaScript loads through an import map (`{{ javascript_importmap() }}`), so there's no
//! bundler or Node toolchain. It maps every `assets/**/*.js` file by its path, Stimulus
//! controllers included (`assets/controllers/hello_controller.js` registers itself as
//! `data-controller="hello"`), plus the pins in `config/importmap.toml`:
//!
//! ```toml
//! "@pierre/diffs" = "vendor/npm/@pierre/diffs@1.5.2.js"   # a file under assets/
//! "canvas-confetti" = "https://cdn.jsdelivr.net/npm/canvas-confetti@1.9.3/+esm"
//! ```
//!
//! `kiln importmap pin <package>` downloads a package from jsDelivr into
//! `assets/vendor/npm/`, which mirrors `cdn.jsdelivr.net/npm/`: `vendor/npm/shiki@4.5.0.js`
//! is mapped as `/npm/shiki@4.5.0/+esm`, the path jsDelivr's builds import it by, so a
//! package's dependencies load from the app too. Other files in `assets/vendor/` are only
//! mapped through pins.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
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

/// Stimulus, vendored (see `vendor/README.md`).
const STIMULUS_VERSION: &str = "3.2.2";
const STIMULUS_JS: &str = include_str!("../vendor/stimulus-3.2.2.js");
const STIMULUS_LICENSE: &str = include_str!("../vendor/stimulus-MIT-LICENSE");
/// Kiln's Stimulus setup: starts it and registers the import map's controllers.
const STIMULUS_LOADER_JS: &str = include_str!("../assets/stimulus.js");

/// Where an app pins import-map modules, relative to its root.
pub(crate) const PINS_FILE: &str = "config/importmap.toml";

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
const MARKDOWN_CSS: &str = include_str!("../assets/markdown.css");
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

/// Upstream's banner has only the copyright line, so the served copy starts with the license.
fn stimulus_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| {
        let header = format!(
            "/*! Stimulus {STIMULUS_VERSION} | https://stimulus.hotwired.dev | vendored and served by Kiln, unmodified below this comment\n\n{}\n*/\n",
            STIMULUS_LICENSE.trim()
        );
        Served::new(&format!("stimulus-{STIMULUS_VERSION}"), "js", format!("{header}{STIMULUS_JS}"))
    })
}

fn stimulus_loader_js() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("kiln-stimulus", "js", STIMULUS_LOADER_JS.to_owned()))
}

/// The URLs of Stimulus and Kiln's setup for it, preloaded on pages with controllers.
pub(crate) fn stimulus_urls() -> [&'static str; 2] {
    [&stimulus_js().url, &stimulus_loader_js().url]
}

/// The `data-controller` name of a controller module: `controllers/admin/users_controller`
/// → `admin--users`, `controllers/bulk_edit_controller` → `bulk-edit` (as Stimulus names
/// them; `assets/stimulus.js` does the same in the browser).
pub(crate) fn controller_identifier(module: &str) -> Option<String> {
    let path = module.strip_prefix("controllers/")?.strip_suffix("_controller")?;
    (!path.is_empty()).then(|| path.replace('/', "--").replace('_', "-"))
}

/// `config/importmap.toml`: a flat table of module names to files under `assets/` or URLs.
fn parse_pins(text: &str) -> Result<BTreeMap<String, String>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{PINS_FILE} isn't valid TOML: {e}"))?;
    table
        .into_iter()
        .map(|(name, value)| match value {
            toml::Value::String(to) => Ok((name, to)),
            other => Err(format!(
                "{PINS_FILE}: `{name}` should be a string (a file under assets/ or a URL), not a {}",
                other.type_str()
            )),
        })
        .collect()
}

fn is_url(to: &str) -> bool {
    to.starts_with("https://") || to.starts_with("http://")
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

fn markdown_css() -> &'static Served {
    static SERVED: OnceLock<Served> = OnceLock::new();
    SERVED.get_or_init(|| Served::new("markdown", "css", MARKDOWN_CSS.to_owned()))
}

/// The stylesheet for `| markdown` content and highlighted code.
pub(crate) fn markdown_css_url() -> &'static str {
    &markdown_css().url
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

/// An asset file as served: its fingerprint and, for a stylesheet, the CSS with its
/// references rewritten.
struct Compiled {
    /// When the file was modified, and its size.
    modified: (SystemTime, u64),
    hash: String,
    /// The rewritten stylesheet (`None` for other files, served as they are on disk).
    css: Option<Arc<[u8]>>,
    /// The assets a stylesheet references (names under `assets/`) and the fingerprints it
    /// was rewritten with (`None`: missing), so it's rebuilt when one of them changes.
    deps: Vec<(String, Option<String>)>,
}

fn is_css(name: &str) -> bool {
    name.len() > 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".css")
}

thread_local! {
    /// The stylesheets being rewritten on this thread, outermost first: a reference back to
    /// one of them is an `@import` cycle.
    static COMPILING: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// `assets/<name>` as served, cached until the file (or, for a stylesheet, a file it
/// references) changes. Embedded files never change, so a release binary hashes each file
/// and rewrites each stylesheet once.
fn compiled(root: &Path, name: &str) -> Option<Arc<Compiled>> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Compiled>>>> = OnceLock::new();
    struct Compiling;
    impl Drop for Compiling {
        fn drop(&mut self) {
            COMPILING.with_borrow_mut(|stack| stack.pop());
        }
    }
    let path = root.join(safe_relative(name)?);
    let modified = if crate::files::embedded() {
        (SystemTime::UNIX_EPOCH, 0)
    } else {
        let meta = std::fs::metadata(&path).ok()?;
        (meta.modified().ok()?, meta.len())
    };
    if COMPILING.with_borrow(|stack| stack.contains(&path)) {
        return None;
    }
    COMPILING.with_borrow_mut(|stack| stack.push(path.clone()));
    let _compiling = Compiling;

    let cache = || CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let cached = cache().get(&path).filter(|c| c.modified == modified).cloned();
    // The lock isn't held while checking references: they look themselves up.
    if let Some(cached) = cached
        && cached.deps.iter().all(|(dep, hash)| compiled(root, dep).map(|d| d.hash.clone()) == *hash)
    {
        return Some(cached);
    }
    let bytes = crate::files::read(&path)?;
    let built = match std::str::from_utf8(&bytes).ok().filter(|_| is_css(name)) {
        Some(css) => {
            let (css, deps) = rewrite_stylesheet(root, name, css);
            Compiled { modified, hash: short_hash(css.as_bytes()), css: Some(css.into_bytes().into()), deps }
        }
        None => Compiled { modified, hash: short_hash(&bytes), css: None, deps: vec![] },
    };
    let built = Arc::new(built);
    cache().insert(path, built.clone());
    Some(built)
}

/// Logs `message` the first time it comes up.
fn warn_once(message: String) {
    static WARNED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    if WARNED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(message.clone()) {
        tracing::warn!(target: "kiln::assets", "{message}");
    }
}

/// `assets/<name>`'s CSS with its references to other assets pointing at their
/// fingerprinted URLs, and the assets it references.
fn rewrite_stylesheet(root: &Path, name: &str, css: &str) -> (String, Vec<(String, Option<String>)>) {
    let mut out = String::with_capacity(css.len() + 64);
    let mut deps: Vec<(String, Option<String>)> = vec![];
    let mut copied = 0;
    for range in css_references(css) {
        let reference = &css[range.clone()];
        let Some((target, suffix)) = css_target(name, reference) else { continue };
        let Some(file) = compiled(root, &target) else {
            if crate::files::is_file(&root.join(&target)) {
                // A cycle: browsers ignore the `@import` that closes it anyway.
                warn_once(format!(
                    "assets/{name}: `{reference}` imports a stylesheet that imports it; left as written"
                ));
            } else {
                deps.push((target.clone(), None));
                warn_once(format!("assets/{name} references `{reference}`, but there's no assets/{target}"));
            }
            continue;
        };
        out.push_str(&css[copied..range.start]);
        out.push_str(&format!("/assets/{}{suffix}", fingerprinted(&target, &file.hash)));
        copied = range.end;
        deps.push((target, Some(file.hash.clone())));
    }
    out.push_str(&css[copied..]);
    (out, deps)
}

/// The asset a stylesheet's reference names, and its `?query`/`#fragment`: relative to the
/// stylesheet (`../fonts/a.ttf` from `css/app.css` is `fonts/a.ttf`), or under `/assets/`
/// (`/assets/fonts/a.ttf`). `None` for anything else: other sites, `data:` URLs, fragments,
/// `public/` files and paths that leave `assets/`.
fn css_target<'a>(stylesheet: &str, reference: &'a str) -> Option<(String, &'a str)> {
    let (path, suffix) = reference.split_at(reference.find(['?', '#']).unwrap_or(reference.len()));
    let scheme = path.find(':').is_some_and(|colon| !path[..colon].contains('/'));
    if path.is_empty() || scheme || path.starts_with("//") {
        return None;
    }
    let mut parts: Vec<&str> = vec![];
    let relative = match path.strip_prefix("/assets/") {
        Some(rest) => rest,
        None if path.starts_with('/') => return None,
        None => {
            parts.extend(stylesheet.split('/'));
            parts.pop();
            path
        }
    };
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty() && !relative.ends_with('/')).then(|| (parts.join("/"), suffix))
}

/// Where the URLs are in a stylesheet: inside `url(..)` and `@import ".."`, quoted or not.
/// Comments and other strings are skipped, and so are URLs with CSS escapes in them.
fn css_references(css: &str) -> Vec<Range<usize>> {
    let b = css.as_bytes();
    // Non-ASCII counts as a name character, as in CSS.
    let is_name = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80;
    let at = |i: usize, word: &str| b.get(i..i + word.len()).is_some_and(|s| s.eq_ignore_ascii_case(word.as_bytes()));
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |end| i + 2 + end + 2);
            }
            b'"' | b'\'' => i = string_end(b, i),
            b'\\' => i += 2,
            b'@' if at(i + 1, "import") && !b.get(i + 7).is_some_and(|&c| is_name(c)) => {
                let start = skip_whitespace(b, i + 7);
                if matches!(b.get(start), Some(b'"' | b'\'')) {
                    let end = string_end(b, start);
                    out.extend(string_body(b, start, end));
                    i = end;
                } else {
                    i = start;
                }
            }
            b'u' | b'U' if at(i, "url(") && (i == 0 || !is_name(b[i - 1])) => match url_body(b, i + 4) {
                Some((body, end)) => {
                    out.push(body);
                    i = end;
                }
                None => i += 4,
            },
            _ => i += 1,
        }
    }
    out
}

fn skip_whitespace(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// Just past the string starting at `start` (at its quote).
fn string_end(b: &[u8], start: usize) -> usize {
    let quote = b[start];
    let mut i = start + 1;
    while let Some(&c) = b.get(i) {
        match c {
            c if c == quote => return i + 1,
            // An unclosed string ends at the line.
            b'\n' => return i,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    b.len()
}

/// Inside the quotes of a closed string with no escapes.
fn string_body(b: &[u8], start: usize, end: usize) -> Option<Range<usize>> {
    let closed = end >= start + 2 && b.get(end - 1) == Some(&b[start]);
    let body = start + 1..end.saturating_sub(1);
    (closed && !b[body.clone()].contains(&b'\\')).then_some(body)
}

/// The URL in `url(..)` (from just past the parenthesis), and where the token ends.
fn url_body(b: &[u8], from: usize) -> Option<(Range<usize>, usize)> {
    let start = skip_whitespace(b, from);
    let (body, after) = if matches!(b.get(start), Some(b'"' | b'\'')) {
        let end = string_end(b, start);
        (string_body(b, start, end)?, end)
    } else {
        let mut end = start;
        while b.get(end).is_some_and(|&c| !c.is_ascii_whitespace() && !b")\"'(\\".contains(&c)) {
            end += 1;
        }
        (start..end, end)
    };
    let close = skip_whitespace(b, after);
    (!body.is_empty() && b.get(close) == Some(&b')')).then_some((body, close + 1))
}

/// The app's directory layout for static files.
#[derive(Debug, Clone)]
pub(crate) struct Assets {
    assets: PathBuf,
    public: PathBuf,
    pins: PathBuf,
    /// In production, the files under `public/` (relative, `/`-separated), listed once so
    /// requests that aren't for one never touch the disk.
    public_files: Arc<OnceLock<BTreeSet<String>>>,
}

impl Assets {
    /// From the app's root directory (the crate with `templates/`, `assets/`, `public/`).
    pub fn at(root: &Path) -> Self {
        Self {
            assets: root.join("assets"),
            public: root.join("public"),
            pins: root.join(PINS_FILE),
            public_files: Arc::default(),
        }
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
        let file = compiled(&self.assets, name)?;
        Some(format!("/assets/{}", fingerprinted(name, &file.hash)))
    }

    /// The stylesheets under `assets/` that reference `name`, directly or through
    /// `@import`s: their URLs change when it does.
    pub(crate) fn stylesheets_using(&self, name: &str) -> Vec<String> {
        fn uses(root: &Path, stylesheet: &str, name: &str, seen: &mut BTreeSet<String>) -> bool {
            let Some(file) = compiled(root, stylesheet) else { return false };
            file.deps
                .iter()
                .any(|(dep, _)| dep == name || (is_css(dep) && seen.insert(dep.clone()) && uses(root, dep, name, seen)))
        }
        let mut found: Vec<String> = crate::files::walk(&self.assets)
            .iter()
            .filter_map(|path| Some(path.strip_prefix(&self.assets).ok()?.to_string_lossy().replace('\\', "/")))
            .filter(|stylesheet| is_css(stylesheet) && uses(&self.assets, stylesheet, name, &mut BTreeSet::new()))
            .collect();
        found.sort();
        found
    }

    /// The pins in `config/importmap.toml` (none if there's no such file).
    pub fn pins(&self) -> Result<BTreeMap<String, String>, String> {
        crate::files::read_string(&self.pins).map_or_else(|| Ok(BTreeMap::new()), |text| parse_pins(&text))
    }

    /// Kiln's modules, every JavaScript file under `assets/` and the app's pins, as
    /// import-map entries: `application` → `/assets/application-…js`,
    /// `controllers/hello_controller` → `/assets/controllers/hello_controller-…js`.
    pub fn importmap(&self) -> Result<BTreeMap<String, String>, String> {
        let mut imports = BTreeMap::from([
            ("@hotwired/turbo".to_owned(), turbo_url().to_owned()),
            ("@hotwired/stimulus".to_owned(), stimulus_js().url.clone()),
            ("kiln/stimulus".to_owned(), stimulus_loader_js().url.clone()),
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
        for path in crate::files::walk(&self.assets) {
            let Ok(relative) = path.strip_prefix(&self.assets) else { continue };
            let name = relative.to_string_lossy().replace('\\', "/");
            let (Some(module), Some(url)) = (name.strip_suffix(".js"), self.url(&name)) else { continue };
            if let Some(package) = module.strip_prefix("vendor/npm/") {
                imports.insert(format!("/npm/{package}/+esm"), url);
            } else if !module.starts_with("vendor/") {
                imports.insert(module.to_owned(), url);
            }
        }
        for (name, to) in self.pins()? {
            let url = if is_url(&to) {
                to
            } else {
                self.url(&to).ok_or_else(|| format!("{PINS_FILE} pins `{name}` to `{to}`, which isn't in assets/"))?
            };
            imports.insert(name, url);
        }
        Ok(imports)
    }

    pub fn has_application_js(&self) -> bool {
        crate::files::is_file(&self.assets.join("application.js"))
    }

    /// Serves `/assets/<fingerprinted name>`.
    pub fn serve_asset(&self, requested: &str) -> Option<Response> {
        let (name, hash) = split_fingerprint(requested)?;
        let path = self.assets.join(safe_relative(&name)?);
        // Stylesheets are served with their references rewritten.
        if is_css(&name)
            && let Some(file) = compiled(&self.assets, &name)
            && let Some(css) = &file.css
        {
            return Some(file_response(&name, css.to_vec(), file.hash == hash));
        }
        let bytes = crate::files::read(&path)?.into_owned();
        // A stale fingerprint still gets the current file, just not cached forever.
        let immutable = short_hash(&bytes) == hash;
        Some(file_response(&name, bytes, immutable))
    }

    /// Serves `public/<path>` when it's a file (not a directory's `index.html`). Checked
    /// before routing, so `/favicon.svg` is the file even beside a `/{owner}` route. In
    /// production the file list is read once; in development the disk is asked, so new
    /// files show up at once.
    pub fn serve_public_file(&self, request_path: &str) -> Option<Response> {
        let name = request_path.strip_prefix('/')?;
        if name.is_empty() || name.ends_with('/') {
            return None;
        }
        if crate::config::is_production() {
            let files = self.public_files.get_or_init(|| {
                crate::files::walk(&self.public)
                    .iter()
                    .filter_map(|path| Some(path.strip_prefix(&self.public).ok()?.to_string_lossy().replace('\\', "/")))
                    .collect()
            });
            if !files.contains(name) {
                return None;
            }
        }
        let path = self.public.join(safe_relative(name)?);
        let bytes = crate::files::read(&path)?.into_owned();
        Some(file_response(name, bytes, false))
    }

    /// Serves a file from `public/`, if there is one.
    pub fn serve_public(&self, request_path: &str) -> Option<Response> {
        let name = request_path.trim_start_matches('/');
        // `/` is `public/index.html` when there is one.
        let mut path = if name.is_empty() { self.public.clone() } else { self.public.join(safe_relative(name)?) };
        if crate::files::is_dir(&path) {
            path = path.join("index.html");
        }
        let bytes = crate::files::read(&path)?.into_owned();
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
        (stimulus_js(), "stimulus.js"),
        (stimulus_loader_js(), "kiln-stimulus.js"),
        (rich_text_js(), "rich-text.js"),
        (rich_text_display_js(), "rich-text-display.js"),
        (confirm_js(), "confirm.js"),
        (direct_upload_js(), "direct-upload.js"),
        (dev_reload_js(), "dev-reload.js"),
        (rich_text_css(), "rich-text.css"),
        (markdown_css(), "markdown.css"),
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

    #[test]
    fn stimulus_is_served_with_its_license() {
        let [stimulus, loader] = stimulus_urls();
        assert!(stimulus.starts_with("/kiln/stimulus-3.2.2-"), "{stimulus}");
        let body = String::from_utf8(stimulus_js().body.clone()).unwrap();
        assert!(body.starts_with("/*! Stimulus 3.2.2") && body.contains("Permission is hereby granted"));
        assert!(body.contains("export { Application,"));
        assert!(serve_kiln(stimulus).is_some() && serve_kiln(loader).is_some());
    }

    #[test]
    fn controllers_are_named_as_stimulus_names_them() {
        assert_eq!(controller_identifier("controllers/hello_controller").as_deref(), Some("hello"));
        assert_eq!(controller_identifier("controllers/admin/users_controller").as_deref(), Some("admin--users"));
        assert_eq!(controller_identifier("controllers/bulk_edit_controller").as_deref(), Some("bulk-edit"));
        assert_eq!(controller_identifier("controllers/helpers"), None);
        assert_eq!(controller_identifier("application"), None);
        assert_eq!(controller_identifier("controllers/_controller"), None);
    }

    /// An app directory with `files` (path, contents), removed on drop.
    struct TempApp(PathBuf);

    impl TempApp {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("kiln-assets-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (path, contents) in files {
                let path = dir.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for TempApp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_import_map_has_controllers_vendored_packages_and_pins() {
        let app = TempApp::new(
            "map",
            &[
                ("assets/application.js", ""),
                ("assets/controllers/hello_controller.js", "export default class {}"),
                ("assets/controllers/admin/users_controller.js", "export default class {}"),
                ("assets/vendor/npm/@pierre/diffs@1.5.2.js", "import \"/npm/shiki@4.5.0/+esm\""),
                ("assets/vendor/npm/shiki@4.5.0.js", ""),
                ("assets/vendor/npm/shiki@4.5.0/wasm.js", ""),
                ("assets/vendor/sortable.js", ""),
                (
                    "config/importmap.toml",
                    "# pins\n\"@pierre/diffs\" = \"vendor/npm/@pierre/diffs@1.5.2.js\"\nsortable = \"vendor/sortable.js\"\n\"canvas-confetti\" = \"https://cdn.jsdelivr.net/npm/canvas-confetti@1.9.3/+esm\"\n",
                ),
            ],
        );
        let imports = Assets::at(&app.0).importmap().unwrap();
        let url =
            |name: &str| imports.get(name).map(String::as_str).unwrap_or_else(|| panic!("no `{name}`: {imports:?}"));
        assert!(url("@hotwired/stimulus").starts_with("/kiln/stimulus-3.2.2-"));
        assert!(url("kiln/stimulus").starts_with("/kiln/kiln-stimulus-"));
        assert!(url("controllers/hello_controller").starts_with("/assets/controllers/hello_controller-"));
        assert!(url("controllers/admin/users_controller").starts_with("/assets/controllers/admin/users_controller-"));
        // jsDelivr's paths, so vendored packages find their dependencies.
        assert!(url("/npm/shiki@4.5.0/+esm").starts_with("/assets/vendor/npm/shiki@4.5.0-"));
        assert!(url("/npm/shiki@4.5.0/wasm/+esm").starts_with("/assets/vendor/npm/shiki@4.5.0/wasm-"));
        assert!(url("@pierre/diffs").starts_with("/assets/vendor/npm/@pierre/diffs@1.5.2-"));
        assert!(url("sortable").starts_with("/assets/vendor/sortable-"));
        assert_eq!(url("canvas-confetti"), "https://cdn.jsdelivr.net/npm/canvas-confetti@1.9.3/+esm");
        // Vendored files are reached through pins, not by path.
        assert!(!imports.keys().any(|k| k.starts_with("vendor/")), "{imports:?}");
    }

    #[test]
    fn pages_start_stimulus_only_when_there_are_controllers() {
        let without = TempApp::new("no-controllers", &[("assets/application.js", "")]);
        let html = crate::view::javascript_importmap(&Assets::at(&without.0)).unwrap().to_string();
        assert!(!html.contains("import \"kiln/stimulus\""), "{html}");
        assert!(!html.contains("modulepreload\" href=\"/kiln/stimulus-"), "only mapped, not preloaded: {html}");

        let with = TempApp::new("controllers", &[("assets/controllers/admin/users_controller.js", "")]);
        let html = crate::view::javascript_importmap(&Assets::at(&with.0)).unwrap().to_string();
        assert!(html.contains("import \"@hotwired/turbo\"; import \"kiln/confirm\";"), "{html}");
        assert!(html.contains(" import \"kiln/stimulus\";</script>"), "{html}");
        for url in stimulus_urls() {
            assert!(html.contains(&format!("<link rel=\"modulepreload\" href=\"{url}\">")), "{html}");
        }
        assert!(html.contains("<link rel=\"modulepreload\" href=\"/assets/controllers/admin/users_controller-"));
    }

    /// The references `css_references` finds, as written.
    fn references(css: &str) -> Vec<&str> {
        css_references(css).into_iter().map(|r| &css[r]).collect()
    }

    #[test]
    fn stylesheet_references_are_found_in_urls_and_imports() {
        assert_eq!(
            references(
                "@import \"a.css\";\n@import 'b.css' layer(x);\n@IMPORT url(c.css);\n\
                 a { background: url(d.png) } b { src: url( 'e.woff2' ) format('woff2'), URL(\"f.ttf\" ) }\n\
                 c { mask: url(\n  g.svg#icon\n) }"
            ),
            ["a.css", "b.css", "c.css", "d.png", "e.woff2", "f.ttf", "g.svg#icon"]
        );
        // Comments, other strings and names ending in `url` aren't references.
        assert_eq!(
            references(
                "/* url(no.png) @import \"no.css\"; */ a::after { content: \"url(no.png)\" } \
                 b { --x: myurl(no.png); y: 'it\\'s url(no.png)' } c { background: url(yes.png) }"
            ),
            ["yes.png"]
        );
        // Escapes, bad unquoted URLs and unclosed tokens are left alone.
        assert_eq!(
            references(
                "a { background: url(\"we\\\"ird.png\") } b { background: url(a b.png) } \
                 c { background: url(x\"y.png) } d { background: url(\"\") } e { background: url(ok.png"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(references("@import\"tight.css\";@importx \"no.css\";"), ["tight.css"]);
        assert_eq!(references("a{b:url(é.png)}/* unclosed"), ["é.png"]);
        assert_eq!(references("a{b:'unclosed\nc:url(ok.png)}"), ["ok.png"]);
    }

    #[test]
    fn stylesheet_references_resolve_inside_assets() {
        let target = |sheet: &str, reference: &'static str| css_target(sheet, reference);
        let some = |name: &str, suffix: &'static str| Some((name.to_owned(), suffix));
        assert_eq!(target("css/tokens.css", "../fonts/a.ttf"), some("fonts/a.ttf", ""));
        assert_eq!(target("css/tokens.css", "./a.css"), some("css/a.css", ""));
        assert_eq!(target("css/tokens.css", "img//b.png?v=1"), some("css/img/b.png", "?v=1"));
        assert_eq!(target("app.css", "icons.svg#close"), some("icons.svg", "#close"));
        assert_eq!(target("css/app.css", "/assets/fonts/a.ttf"), some("fonts/a.ttf", ""));
        for untouched in [
            "https://example.com/a.ttf",
            "http://example.com/a.ttf",
            "//cdn.example.com/a.ttf",
            "data:image/png;base64,AAAA",
            "#gradient",
            "?v=1",
            "/favicon.svg",
            "/assetsx/a.png",
            "../../a.png",
            "fonts/",
        ] {
            assert_eq!(target("css/app.css", untouched), None, "{untouched}");
        }
        assert_eq!(target("app.css", "../a.png"), None);
    }

    /// Sets `path`'s modification time, so tests don't depend on the clock's resolution.
    fn touch(path: &Path, contents: &str, secs: u64) {
        std::fs::write(path, contents).unwrap();
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + secs);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
    }

    fn body(response: Response) -> String {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let bytes = runtime.block_on(axum::body::to_bytes(response.into_body(), usize::MAX)).unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn stylesheets_point_at_fingerprinted_assets() {
        let app = TempApp::new(
            "css",
            &[
                ("assets/app.css", "@import \"css/tokens.css\";\nbody { background: url(img/bg.png?v=2) }\n"),
                (
                    "assets/css/tokens.css",
                    "@font-face { src: url(\"../fonts/Atlas.ttf\") format(\"truetype\"), url('/assets/fonts/Atlas.svg#atlas') }\n\
                     a { background: url(https://example.com/x.png), url(data:image/gif;base64,R0lG), url(/favicon.svg), url(missing.png) }\n",
                ),
                ("assets/fonts/Atlas.ttf", "font"),
                ("assets/fonts/Atlas.svg", "<svg/>"),
                ("assets/img/bg.png", "png"),
            ],
        );
        let assets = Assets::at(&app.0);
        let font = assets.url("fonts/Atlas.ttf").unwrap();
        let svg = assets.url("fonts/Atlas.svg").unwrap();
        let tokens_url = assets.url("css/tokens.css").unwrap();
        let tokens = body(assets.serve_asset(tokens_url.strip_prefix("/assets/").unwrap()).unwrap());
        assert_eq!(
            tokens,
            format!(
                "@font-face {{ src: url(\"{font}\") format(\"truetype\"), url('{svg}#atlas') }}\n\
                 a {{ background: url(https://example.com/x.png), url(data:image/gif;base64,R0lG), url(/favicon.svg), url(missing.png) }}\n"
            )
        );
        let app_url = assets.url("app.css").unwrap();
        let response = assets.serve_asset(app_url.strip_prefix("/assets/").unwrap()).unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
        let css = body(response);
        let bg = assets.url("img/bg.png").unwrap();
        assert_eq!(css, format!("@import \"{tokens_url}\";\nbody {{ background: url({bg}?v=2) }}\n"));
        // The fingerprint is of what's served.
        assert_eq!(app_url, format!("/assets/{}", fingerprinted("app.css", &short_hash(css.as_bytes()))));
        assert_eq!(assets.stylesheets_using("fonts/Atlas.ttf"), ["app.css", "css/tokens.css"]);
        assert_eq!(assets.stylesheets_using("css/tokens.css"), ["app.css"]);
        assert!(assets.stylesheets_using("img/nope.png").is_empty());

        // A font changing changes the stylesheets that use it, through imports too.
        touch(&app.0.join("assets/fonts/Atlas.ttf"), "font v2", 1);
        let font_v2 = assets.url("fonts/Atlas.ttf").unwrap();
        assert_ne!(font, font_v2);
        let tokens_v2 = assets.url("css/tokens.css").unwrap();
        assert_ne!(tokens_url, tokens_v2);
        let app_v2 = assets.url("app.css").unwrap();
        assert_ne!(app_url, app_v2);
        assert!(body(assets.serve_asset(app_v2.strip_prefix("/assets/").unwrap()).unwrap()).contains(&tokens_v2));
        // The old URL still answers, with the current CSS, but isn't cached.
        let stale = assets.serve_asset(app_url.strip_prefix("/assets/").unwrap()).unwrap();
        assert_eq!(stale.headers()[header::CACHE_CONTROL], "no-cache");

        // A missing file appearing is picked up as well.
        touch(&app.0.join("assets/css/missing.png"), "png", 2);
        let missing = assets.url("css/missing.png").unwrap();
        let tokens_v3 = assets.url("css/tokens.css").unwrap();
        let css = body(assets.serve_asset(tokens_v3.strip_prefix("/assets/").unwrap()).unwrap());
        assert!(css.contains(&format!("url({missing})")), "{css}");
    }

    #[test]
    fn import_cycles_are_left_as_written() {
        let app = TempApp::new(
            "css-cycle",
            &[
                ("assets/a.css", "@import \"b.css\";\na { color: red }\n"),
                ("assets/b.css", "@import url(a.css);\n@import 'b.css';\nb { color: blue }\n"),
            ],
        );
        let assets = Assets::at(&app.0);
        let a = assets.url("a.css").unwrap();
        let b = assets.url("b.css").unwrap();
        let a_css = body(assets.serve_asset(a.strip_prefix("/assets/").unwrap()).unwrap());
        assert_eq!(a_css, format!("@import \"{b}\";\na {{ color: red }}\n"));
        let b_css = body(assets.serve_asset(b.strip_prefix("/assets/").unwrap()).unwrap());
        assert_eq!(b_css, "@import url(a.css);\n@import 'b.css';\nb { color: blue }\n");
        // Stable across lookups.
        assert_eq!(assets.url("a.css").unwrap(), a);
        assert_eq!(assets.url("b.css").unwrap(), b);
    }

    #[test]
    fn bad_pins_are_errors() {
        let missing = TempApp::new("missing-pin", &[("config/importmap.toml", "diffs = \"vendor/diffs.js\"\n")]);
        let error = Assets::at(&missing.0).importmap().unwrap_err();
        assert!(error.contains("pins `diffs` to `vendor/diffs.js`, which isn't in assets/"), "{error}");
        let table = TempApp::new("table-pin", &[("config/importmap.toml", "[diffs]\nto = \"x\"\n")]);
        let error = Assets::at(&table.0).importmap().unwrap_err();
        assert!(error.contains("`diffs` should be a string"), "{error}");
        let none = TempApp::new("no-pins", &[]);
        assert!(Assets::at(&none.0).pins().unwrap().is_empty());
    }
}
