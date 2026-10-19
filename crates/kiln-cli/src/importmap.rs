//! `kiln importmap pin|unpin`: JavaScript packages for the import map, without npm.
//!
//! `pin` downloads a package's ES module build from jsDelivr (`/npm/<pkg>@<version>/+esm`)
//! and the modules it imports into `assets/vendor/npm/`, which mirrors jsDelivr's `/npm/`
//! paths (kiln maps `vendor/npm/shiki@4.5.0.js` as `/npm/shiki@4.5.0/+esm`, the specifier
//! jsDelivr's builds import it by), then pins the package in `config/importmap.toml`. Pages
//! load it from the app like its own JavaScript. Modules a package only loads on demand
//! (`import("..")`, such as syntax grammars) aren't downloaded: a `"/npm/"` pin sends them
//! to jsDelivr, and `pin` says which there are. Pinning one by its subpath
//! (`@shikijs/langs/rust`, or `@shikijs/langs/rust,tsx` for several) downloads it at the
//! versions the vendored files ask for, so their `import("/npm/@shikijs/langs@4.5.0/rust/+esm")`
//! loads from the app too. `--cdn` pins the jsDelivr URL instead of downloading.
//!
//! `assets/vendor/npm/` belongs to this command: files no pin reaches are deleted.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::Subcommand;

use crate::app::App;

const CDN: &str = "https://cdn.jsdelivr.net";
pub const PINS: &str = "config/importmap.toml";
/// Where downloaded modules go, relative to `assets/`.
const VENDOR: &str = "vendor/npm";
const HEADER: &str = "# Import map pins: a module name and where it loads from, a file under assets/ or a URL.\n\
# `kiln importmap pin <package>` downloads a package into assets/vendor/npm/ and pins it here.\n";
/// The pin that sends modules vendored packages load on demand to jsDelivr.
const LAZY_KEY: &str = "/npm/";
const LAZY_COMMENT: &str = "# Modules the vendored packages load on demand come from jsDelivr.";

#[derive(Subcommand)]
pub enum Importmap {
    /// Download packages from jsDelivr into assets/vendor/npm/ and pin them in
    /// config/importmap.toml: `kiln importmap pin @pierre/diffs` (the latest version),
    /// `canvas-confetti@1.9.3`, `lodash-es@^4`. Then `import { .. } from "@pierre/diffs"`.
    /// A module inside a package, such as one a vendored package loads on demand:
    /// `@shikijs/langs/rust`, `@shikijs/langs@4.5.0/rust,tsx`, `@shikijs/themes/github-dark`.
    Pin {
        #[arg(required = true)]
        packages: Vec<String>,
        /// Pin the jsDelivr URL instead of downloading (pages then load it from the CDN).
        #[arg(long)]
        cdn: bool,
    },
    /// Remove packages' (or modules') pins, and the downloaded files nothing else uses.
    Unpin {
        #[arg(required = true)]
        packages: Vec<String>,
    },
}

impl Importmap {
    pub fn run(self, app: &App) -> Result<(), String> {
        let log = match self {
            Importmap::Pin { packages, cdn } => {
                let mut log = vec![];
                for spec in &packages {
                    log.extend(pin(&app.root, spec, cdn, &Curl)?);
                }
                log
            }
            Importmap::Unpin { packages } => unpin(&app.root, &packages)?,
        };
        for line in log {
            println!("{line}");
        }
        Ok(())
    }
}

/// Gets a URL's body. Tests stand in for the network with a map.
pub trait Fetch {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// curl, which every platform kiln runs on has, so the CLI carries no HTTP client.
struct Curl;

impl Fetch for Curl {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let output = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location", "--max-time", "120", url])
            .output()
            .map_err(|e| format!("couldn't run curl to download {url}: {e}"))?;
        if !output.status.success() {
            return Err(format!("downloading {url} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(output.stdout)
    }
}

/// What `pin` and `unpin` were asked for: `@shikijs/langs@4.5.0/rust,typescript` is the
/// package `@shikijs/langs`, version `4.5.0`, modules `rust` and `typescript`.
#[derive(Debug, PartialEq)]
struct Spec<'a> {
    name: &'a str,
    /// `None`: the latest, or for a module, the version the vendored packages ask for.
    version: Option<&'a str>,
    /// Modules inside the package (`rust`, `dist/x.mjs`); none for the package itself. A comma
    /// list in the last segment names several: `@shikijs/themes/github-dark,github-light`.
    subpaths: Vec<String>,
}

fn parse_spec(spec: &str) -> Spec<'_> {
    let name_start = if spec.starts_with('@') { spec.find('/').map_or(spec.len(), |i| i + 1) } else { 0 };
    let name_end = spec[name_start..].find(['@', '/']).map_or(spec.len(), |i| name_start + i);
    let (name, rest) = spec.split_at(name_end);
    let (version, rest) = match rest.strip_prefix('@') {
        Some(after) => match after.find('/') {
            Some(i) => (Some(&after[..i]), &after[i..]),
            None => (Some(after), ""),
        },
        None => (None, rest),
    };
    let subpaths = match rest.strip_prefix('/') {
        Some(path) => {
            let (dir, last) = path.rsplit_once('/').map_or(("", path), |(d, l)| (d, l));
            last.split(',')
                .map(str::trim)
                .map(|m| if dir.is_empty() { m.to_owned() } else { format!("{dir}/{m}") })
                .collect()
        }
        None => vec![],
    };
    Spec { name, version: version.filter(|v| !v.is_empty()), subpaths }
}

impl Spec<'_> {
    /// The keys in config/importmap.toml: `@shikijs/langs/rust`, or the package's name.
    fn keys(&self) -> Vec<String> {
        if self.subpaths.is_empty() {
            return vec![self.name.to_owned()];
        }
        self.subpaths.iter().map(|s| format!("{}/{s}", self.name)).collect()
    }
}

fn valid_package(name: &str) -> bool {
    let bare = name.strip_prefix('@').map_or(Some(name), |scoped| scoped.split_once('/').map(|(_, n)| n));
    !name.is_empty()
        && bare.is_some_and(|b| !b.is_empty() && !b.contains('/'))
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"@/._-~".contains(&b))
        && !name.split('/').any(|part| part == "." || part == "..")
}

/// The exact version jsDelivr serves for a version or range (`latest`, `^4`, `1.5.2`).
fn resolve(name: &str, version: &str, fetch: &dyn Fetch) -> Result<String, String> {
    let url = format!("https://data.jsdelivr.com/v1/packages/npm/{name}/resolved?specifier={version}");
    let body = fetch.fetch(&url).map_err(|e| format!("couldn't find `{name}` on jsDelivr ({e})"))?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("jsDelivr's answer for `{name}` isn't JSON ({e})"))?;
    json["version"].as_str().map(str::to_owned).ok_or_else(|| format!("`{name}` has no version matching `{version}`"))
}

/// `/npm/shiki@4.5.0/wasm/+esm` → `vendor/npm/shiki@4.5.0/wasm.js`, for safe paths only.
fn vendor_path(specifier: &str) -> Option<String> {
    let path = specifier.strip_prefix("/npm/")?.strip_suffix("/+esm")?;
    let safe = path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"@._-~+".contains(&b))
    });
    safe.then(|| format!("{VENDOR}/{path}.js"))
}

/// `vendor/npm/shiki@4.5.0.js` → `/npm/shiki@4.5.0/+esm`.
fn specifier_of(vendor_path: &str) -> Option<String> {
    let path = vendor_path.strip_prefix(&format!("{VENDOR}/"))?.strip_suffix(".js")?;
    Some(format!("/npm/{path}/+esm"))
}

/// The jsDelivr modules a build imports: `(specifier, loaded on demand)`. jsDelivr writes
/// them as absolute `/npm/..+esm` strings after `from`, `import` or `import(`.
fn imports(source: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    for quote in ['"', '\''] {
        let needle = format!("{quote}/npm/");
        let mut rest = source;
        let mut offset = 0;
        while let Some(i) = rest.find(&needle) {
            let start = offset + i + 1;
            let Some(len) = source[start..].find(quote) else { break };
            let specifier = &source[start..start + len];
            let before = source[..start - 1].trim_end();
            let dynamic = before.ends_with("import(");
            if specifier.ends_with("/+esm") && (dynamic || before.ends_with("from") || before.ends_with("import")) {
                out.push((specifier.to_owned(), dynamic));
            }
            offset = start + len;
            rest = &source[offset..];
        }
    }
    out
}

/// Downloads `specifier` and everything it imports up front, reusing files already there
/// (a versioned build doesn't change). The modules it loads on demand that aren't vendored,
/// which are left to jsDelivr.
fn download(assets: &Path, specifier: &str, fetch: &dyn Fetch, log: &mut Vec<String>) -> Result<Vec<String>, String> {
    let mut queue = vec![specifier.to_owned()];
    let mut seen = BTreeSet::new();
    let mut lazy = BTreeSet::new();
    while let Some(specifier) = queue.pop() {
        if !seen.insert(specifier.clone()) {
            continue;
        }
        let relative = vendor_path(&specifier).ok_or_else(|| format!("unexpected module path `{specifier}`"))?;
        let path = assets.join(&relative);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(_) => {
                let bytes = fetch.fetch(&format!("{CDN}{specifier}"))?;
                let source = String::from_utf8(bytes).map_err(|_| format!("{CDN}{specifier} isn't UTF-8"))?;
                // The source map lives on jsDelivr; a same-origin `/sm/..` URL would 404.
                let source: String = source
                    .lines()
                    .filter(|l| !l.starts_with("//# sourceMappingURL="))
                    .flat_map(|l| [l, "\n"])
                    .collect();
                fs::create_dir_all(path.parent().expect("vendor files are in a directory"))
                    .map_err(|e| e.to_string())?;
                fs::write(&path, &source).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
                log.push(format!("create  assets/{relative}"));
                source
            }
        };
        for (import, dynamic) in imports(&source) {
            if !dynamic {
                queue.push(import);
            } else if !vendor_path(&import).is_some_and(|p| assets.join(p).is_file()) {
                lazy.insert(import);
            }
        }
    }
    Ok(lazy.into_iter().collect())
}

/// Every jsDelivr module the vendored files ask for, statically or on demand.
fn requested(assets: &Path) -> BTreeSet<String> {
    files_under(&assets.join(VENDOR))
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|source| imports(&source).into_iter().map(|(specifier, _)| specifier).collect::<Vec<_>>())
        .collect()
}

/// The versions of `name`'s module `subpath` that `requested` asks for:
/// `/npm/@shikijs/langs@4.5.0/rust/+esm` → `4.5.0`.
fn requested_versions(requested: &BTreeSet<String>, name: &str, subpath: &str) -> Vec<String> {
    let prefix = format!("/npm/{name}@");
    let suffix = format!("/{subpath}/+esm");
    let mut versions: Vec<String> = requested
        .iter()
        .filter_map(|s| s.strip_prefix(&prefix)?.strip_suffix(&suffix))
        .filter(|v| !v.is_empty() && !v.contains('/'))
        .map(str::to_owned)
        .collect();
    versions.sort_by_key(|v| version_order(v));
    versions
}

/// Sorts `4.10.0` after `4.5.0`; pre-release tags compare as text.
fn version_order(version: &str) -> Vec<(u64, String)> {
    version
        .split(['.', '-', '+'])
        .map(|part| match part.parse::<u64>() {
            Ok(n) => (n, String::new()),
            Err(_) => (0, part.to_owned()),
        })
        .collect()
}

/// `/npm/@shikijs/langs@4.5.0/rust/+esm` → (`@shikijs/langs@4.5.0`, `rust`).
fn module_of(specifier: &str) -> Option<(&str, &str)> {
    let path = specifier.strip_prefix("/npm/")?.strip_suffix("/+esm")?;
    let skip = if path.starts_with('@') { 2 } else { 1 };
    let split = path.match_indices('/').nth(skip - 1).map(|(i, _)| i)?;
    Some((&path[..split], &path[split + 1..]))
}

/// One line per package whose modules load on demand from jsDelivr, saying how to vendor them.
fn on_demand(lazy: &[String]) -> Vec<String> {
    let mut by_package: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for specifier in lazy {
        if let Some((package, module)) = module_of(specifier) {
            by_package.entry(package).or_default().push(module);
        }
    }
    by_package
        .into_iter()
        .map(|(package, modules)| {
            let examples: Vec<&str> = modules.iter().take(3).copied().collect();
            let more = if modules.len() > 3 { ", .." } else { "" };
            let name = package.rsplit_once('@').map_or(package, |(n, _)| n);
            format!(
                "on demand from jsDelivr: {} module(s) of {package} ({}{more}); vendor the ones you use with \
                 `kiln importmap pin {name}/<module>[,<module>..]`",
                modules.len(),
                examples.join(", "),
            )
        })
        .collect()
}

/// `kiln importmap pin <package>[@version][/module,..]`: the lines to print.
pub fn pin(root: &Path, spec: &str, cdn: bool, fetch: &dyn Fetch) -> Result<Vec<String>, String> {
    let spec = parse_spec(spec);
    let name = spec.name;
    if !valid_package(name) {
        return Err(format!("`{name}` isn't an npm package name (`canvas-confetti`, `@pierre/diffs`)"));
    }
    let assets = root.join("assets");
    let mut log = vec![];
    let text = fs::read_to_string(root.join(PINS)).unwrap_or_else(|_| HEADER.to_owned());
    let mut lazy_specifiers = vec![];
    // (key, where it loads from, what to say)
    let mut set: Vec<(String, String, String)> = vec![];
    if spec.subpaths.is_empty() {
        let version = resolve(name, spec.version.unwrap_or("latest"), fetch)?;
        let specifier = format!("/npm/{name}@{version}/+esm");
        let to = if cdn {
            format!("{CDN}{specifier}")
        } else {
            lazy_specifiers = download(&assets, &specifier, fetch, &mut log)?;
            vendor_path(&specifier).expect("a valid package name makes a valid path")
        };
        set.push((name.to_owned(), to, format!("pinned {name}@{version}: `import .. from \"{name}\"`")));
    } else {
        let wanted = requested(&assets);
        for subpath in &spec.subpaths {
            let key = format!("{name}/{subpath}");
            let asked = requested_versions(&wanted, name, subpath);
            // A module the vendored packages load on demand is wanted at the versions they ask
            // for, else the latest.
            let versions = match spec.version {
                Some(version) => vec![resolve(name, version, fetch)?],
                None if !asked.is_empty() => asked.clone(),
                None => vec![resolve(name, "latest", fetch)?],
            };
            let mut to = String::new();
            for version in &versions {
                let specifier = format!("/npm/{name}@{version}/{subpath}/+esm");
                if vendor_path(&specifier).is_none() {
                    return Err(format!("`{key}` isn't a module path (`@shikijs/langs/rust`)"));
                }
                to = if cdn {
                    format!("{CDN}{specifier}")
                } else {
                    lazy_specifiers.extend(download(&assets, &specifier, fetch, &mut log)?);
                    vendor_path(&specifier).expect("checked above")
                };
                if !asked.is_empty() && !asked.contains(version) {
                    log.push(format!(
                        "note: the vendored packages load {name}@{}/{subpath}, not @{version}; pin that \
                         version for them to use this copy",
                        asked.join(", @")
                    ));
                }
            }
            let used_by = if asked.iter().any(|v| versions.contains(v)) {
                let calls: Vec<String> = versions
                    .iter()
                    .filter(|v| asked.contains(v))
                    .map(|v| format!("`import(\"/npm/{name}@{v}/{subpath}/+esm\")`"))
                    .collect();
                format!(", and the vendored packages' {} load from the app", calls.join(" and "))
            } else {
                String::new()
            };
            let version = versions.last().expect("at least one version");
            set.push((
                key.clone(),
                to,
                format!("pinned {name}@{version}/{subpath}: `import .. from \"{key}\"`{used_by}"),
            ));
        }
    }
    let lazy = pins(&text)?.contains_key(LAZY_KEY) || !lazy_specifiers.is_empty();
    // The on-demand fallback stays last, after the packages it serves.
    let mut text = without_lazy(&text);
    for (key, to, _) in &set {
        text = set_pin(&text, key, to);
    }
    if lazy {
        text = format!(
            "{text}\n{LAZY_COMMENT}\n{} = {}\n",
            toml::Value::from(LAZY_KEY),
            toml::Value::from(format!("{CDN}/npm/"))
        );
    }
    write_pins(root, &text, &mut log)?;
    log.extend(prune(root, &text)?);
    log.extend(set.into_iter().map(|(_, _, said)| said));
    log.extend(on_demand(&lazy_specifiers));
    Ok(log)
}

/// `kiln importmap unpin <package>..` (or `<package>/<module>`): the lines to print.
pub fn unpin(root: &Path, names: &[String]) -> Result<Vec<String>, String> {
    let mut text = fs::read_to_string(root.join(PINS)).map_err(|_| format!("there's no {PINS}"))?;
    let pinned = pins(&text)?;
    let assets = root.join("assets");
    let mut log = vec![];
    for spec in names {
        let spec = parse_spec(spec);
        for key in spec.keys() {
            if !pinned.contains_key(&key) {
                return Err(format!("`{key}` isn't pinned in {PINS}"));
            }
            text = remove_pin(&text, &key);
        }
        // A module's files go even though the package that loads it on demand still reaches
        // them; it loads from jsDelivr again.
        for subpath in &spec.subpaths {
            let prefix = format!("{VENDOR}/{}@", spec.name);
            let suffix = format!("/{subpath}.js");
            for path in files_under(&assets.join(VENDOR)) {
                let relative = path.strip_prefix(&assets).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                let version = relative.strip_prefix(&prefix).and_then(|r| r.strip_suffix(&suffix));
                if version.is_some_and(|v| !v.is_empty() && !v.contains('/')) {
                    fs::remove_file(&path).map_err(|e| format!("couldn't remove {}: {e}", path.display()))?;
                    log.push(format!("remove  assets/{relative}"));
                }
            }
        }
    }
    // Nothing left to load on demand for.
    if !pins(&text)?.values().any(|to| to.starts_with(&format!("{VENDOR}/"))) {
        text = without_lazy(&text);
    }
    write_pins(root, &text, &mut log)?;
    log.extend(prune(root, &text)?);
    Ok(log)
}

/// The pins in `config/importmap.toml`'s text.
pub fn pins(text: &str) -> Result<BTreeMap<String, String>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{PINS} isn't valid TOML: {e}"))?;
    Ok(table.into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_owned()))).collect())
}

/// The key a line of the file sets, if it sets one.
fn line_key(line: &str) -> Option<String> {
    line.parse::<toml::Table>().ok()?.keys().next().cloned()
}

fn remove_pin(text: &str, name: &str) -> String {
    text.lines().filter(|l| line_key(l).as_deref() != Some(name)).flat_map(|l| [l, "\n"]).collect()
}

fn without_lazy(text: &str) -> String {
    let kept = remove_pin(text, LAZY_KEY).replace(&format!("{LAZY_COMMENT}\n"), "");
    format!("{}\n", kept.trim_end())
}

/// Replaces `name`'s line, or adds one at the end.
fn set_pin(text: &str, name: &str, to: &str) -> String {
    let line = format!("{} = {}", toml::Value::from(name), toml::Value::from(to));
    if text.lines().any(|l| line_key(l).as_deref() == Some(name)) {
        let lines = text.lines().map(|l| if line_key(l).as_deref() == Some(name) { line.as_str() } else { l });
        return lines.flat_map(|l| [l, "\n"]).collect();
    }
    let mut out = text.trim_end().to_owned();
    out.push('\n');
    out.push_str(&line);
    out.push('\n');
    out
}

fn write_pins(root: &Path, text: &str, log: &mut Vec<String>) -> Result<(), String> {
    let path = root.join(PINS);
    if fs::read_to_string(&path).ok().as_deref() == Some(text) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("config/")).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| format!("couldn't write {PINS}: {e}"))?;
    log.push(format!("update  {PINS}"));
    Ok(())
}

/// Deletes downloaded files that no pin reaches any more (an old version, an unpinned
/// package's dependencies).
fn prune(root: &Path, text: &str) -> Result<Vec<String>, String> {
    let assets = root.join("assets");
    let mut queue: Vec<String> = pins(text)?.values().filter_map(|to| specifier_of(to)).collect();
    let mut reached = BTreeSet::new();
    while let Some(specifier) = queue.pop() {
        let Some(relative) = vendor_path(&specifier) else { continue };
        if !reached.insert(relative.clone()) {
            continue;
        }
        let source = fs::read_to_string(assets.join(&relative)).unwrap_or_default();
        queue.extend(imports(&source).into_iter().map(|(import, _)| import));
    }
    let mut log = vec![];
    for path in files_under(&assets.join(VENDOR)) {
        let relative = path.strip_prefix(&assets).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if !reached.contains(&relative) {
            fs::remove_file(&path).map_err(|e| format!("couldn't remove {}: {e}", path.display()))?;
            log.push(format!("remove  assets/{relative}"));
        }
    }
    remove_empty_dirs(&assets.join(VENDOR));
    Ok(log)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn remove_empty_dirs(dir: &Path) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            remove_empty_dirs(&entry.path());
        }
    }
    // Fails (and is meant to) unless the directory is empty.
    let _ = fs::remove_dir(dir);
}

/// The app's Stimulus controllers: `(identifier, file)`, e.g. `("admin--users",
/// "assets/controllers/admin/users_controller.js")`.
pub fn controllers(root: &Path) -> Vec<(String, String)> {
    let assets = root.join("assets");
    files_under(&assets.join("controllers"))
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(&assets).ok()?.to_string_lossy().replace('\\', "/");
            let name = relative.strip_prefix("controllers/")?.strip_suffix("_controller.js")?;
            let identifier = name.replace('/', "--").replace('_', "-");
            (!name.is_empty()).then(|| (identifier, format!("assets/{relative}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use super::*;

    /// jsDelivr, as a map. Records what was asked for.
    struct FakeCdn {
        files: HashMap<String, String>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeCdn {
        fn new(files: &[(&str, &str)]) -> Self {
            let files = files.iter().map(|(url, body)| (url.to_string(), body.to_string())).collect();
            Self { files, asked: RefCell::default() }
        }
    }

    impl Fetch for FakeCdn {
        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.asked.borrow_mut().push(url.to_owned());
            self.files.get(url).map(|b| b.clone().into_bytes()).ok_or_else(|| format!("404 for {url}"))
        }
    }

    fn resolved(name: &str, version: &str, answer: &str) -> (String, String) {
        (
            format!("https://data.jsdelivr.com/v1/packages/npm/{name}/resolved?specifier={version}"),
            format!("{{\"type\":\"npm\",\"name\":\"{name}\",\"version\":\"{answer}\"}}"),
        )
    }

    fn temp_app(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-importmap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("assets")).unwrap();
        dir
    }

    #[test]
    fn splits_package_specs() {
        let spec = |name, version, subpaths: &[&str]| Spec {
            name,
            version,
            subpaths: subpaths.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(parse_spec("@pierre/diffs@1.5.2"), spec("@pierre/diffs", Some("1.5.2"), &[]));
        assert_eq!(parse_spec("@pierre/diffs"), spec("@pierre/diffs", None, &[]));
        assert_eq!(parse_spec("lodash-es@^4"), spec("lodash-es", Some("^4"), &[]));
        assert_eq!(parse_spec("canvas-confetti"), spec("canvas-confetti", None, &[]));
        assert_eq!(parse_spec("@shikijs/langs@4.5.0/rust"), spec("@shikijs/langs", Some("4.5.0"), &["rust"]));
        assert_eq!(parse_spec("@shikijs/langs/rust,tsx"), spec("@shikijs/langs", None, &["rust", "tsx"]));
        assert_eq!(parse_spec("lodash-es/debounce"), spec("lodash-es", None, &["debounce"]));
        assert_eq!(parse_spec("pkg@1/dist/a.mjs,b.mjs"), spec("pkg", Some("1"), &["dist/a.mjs", "dist/b.mjs"]));
        assert_eq!(parse_spec("@shikijs/langs/rust,tsx").keys(), ["@shikijs/langs/rust", "@shikijs/langs/tsx"]);
        assert_eq!(module_of("/npm/@shikijs/langs@4.5.0/rust/+esm"), Some(("@shikijs/langs@4.5.0", "rust")));
        assert_eq!(module_of("/npm/shiki@4.5.0/wasm/+esm"), Some(("shiki@4.5.0", "wasm")));
        assert_eq!(module_of("/npm/shiki@4.5.0/+esm"), None);
        let mut versions = vec!["4.10.0", "4.5.0", "4.5.0-beta.1"];
        versions.sort_by_key(|v| version_order(v));
        assert_eq!(versions, ["4.5.0", "4.5.0-beta.1", "4.10.0"]);
        assert!(valid_package("@pierre/diffs") && valid_package("lodash.debounce"));
        assert!(!valid_package("../x") && !valid_package("@scope") && !valid_package("a/b") && !valid_package(""));
    }

    #[test]
    fn finds_jsdelivr_imports() {
        let source = r#"import{a as b}from"/npm/shiki@4.5.0/+esm";import"/npm/side@1.0.0/+esm";export*from '/npm/re@2.0.0/x/+esm';const l=()=>import("/npm/@shikijs/langs@4.5.0/abap/+esm");const s="/npm/not-an-import@1/+esm";"#;
        assert_eq!(
            imports(source),
            [
                ("/npm/shiki@4.5.0/+esm".to_owned(), false),
                ("/npm/side@1.0.0/+esm".to_owned(), false),
                ("/npm/@shikijs/langs@4.5.0/abap/+esm".to_owned(), true),
                ("/npm/re@2.0.0/x/+esm".to_owned(), false),
            ]
        );
        assert_eq!(vendor_path("/npm/shiki@4.5.0/wasm/+esm").as_deref(), Some("vendor/npm/shiki@4.5.0/wasm.js"));
        assert_eq!(vendor_path("/npm/../../etc/+esm"), None);
        assert_eq!(specifier_of("vendor/npm/@pierre/diffs@1.5.2.js").as_deref(), Some("/npm/@pierre/diffs@1.5.2/+esm"));
    }

    #[test]
    fn pin_downloads_the_package_and_what_it_imports() {
        let root = temp_app("pin");
        let (resolve_url, resolve_body) = resolved("@pierre/diffs", "latest", "1.5.2");
        let cdn = FakeCdn::new(&[
            (&resolve_url, &resolve_body),
            (
                "https://cdn.jsdelivr.net/npm/@pierre/diffs@1.5.2/+esm",
                "import{h}from\"/npm/shiki@4.5.0/+esm\";export const d=1;\n//# sourceMappingURL=/sm/abc.map",
            ),
            (
                "https://cdn.jsdelivr.net/npm/shiki@4.5.0/+esm",
                "export const h=()=>import(\"/npm/@shikijs/langs@4.5.0/abap/+esm\");",
            ),
        ]);
        let log = pin(&root, "@pierre/diffs", false, &cdn).unwrap();
        assert!(log.contains(&"create  assets/vendor/npm/@pierre/diffs@1.5.2.js".to_owned()), "{log:?}");
        assert!(log.contains(&"create  assets/vendor/npm/shiki@4.5.0.js".to_owned()), "{log:?}");
        let entry = fs::read_to_string(root.join("assets/vendor/npm/@pierre/diffs@1.5.2.js")).unwrap();
        assert!(entry.contains("export const d=1;") && !entry.contains("sourceMappingURL"), "{entry}");
        // The grammar loads on demand, so it isn't downloaded; the CDN pin covers it.
        assert!(!cdn.asked.borrow().iter().any(|u| u.contains("langs")));
        let text = fs::read_to_string(root.join(PINS)).unwrap();
        let pinned = pins(&text).unwrap();
        assert_eq!(pinned["@pierre/diffs"], "vendor/npm/@pierre/diffs@1.5.2.js");
        assert_eq!(pinned["/npm/"], "https://cdn.jsdelivr.net/npm/");
        assert!(text.starts_with("# Import map pins"), "{text}");

        // A new version replaces the old one's files; shared dependencies stay.
        let (resolve_url, resolve_body) = resolved("@pierre/diffs", "1.6.0", "1.6.0");
        let cdn = FakeCdn::new(&[
            (&resolve_url, &resolve_body),
            ("https://cdn.jsdelivr.net/npm/@pierre/diffs@1.6.0/+esm", "import{h}from\"/npm/shiki@4.5.0/+esm\";"),
        ]);
        let log = pin(&root, "@pierre/diffs@1.6.0", false, &cdn).unwrap();
        assert!(log.contains(&"remove  assets/vendor/npm/@pierre/diffs@1.5.2.js".to_owned()), "{log:?}");
        assert!(root.join("assets/vendor/npm/shiki@4.5.0.js").is_file());
        let text = fs::read_to_string(root.join(PINS)).unwrap();
        // The on-demand fallback stays last, after the packages.
        assert!(text.ends_with(&format!("{LAZY_COMMENT}\n\"/npm/\" = \"https://cdn.jsdelivr.net/npm/\"\n")), "{text}");
        let pinned = pins(&text).unwrap();
        assert_eq!(pinned["@pierre/diffs"], "vendor/npm/@pierre/diffs@1.6.0.js");
        assert_eq!(pinned.len(), 2, "{pinned:?}");

        // Unpinning removes the pin, the files and the CDN fallback.
        unpin(&root, &["@pierre/diffs".to_owned()]).unwrap();
        assert!(pins(&fs::read_to_string(root.join(PINS)).unwrap()).unwrap().is_empty());
        assert!(!root.join("assets/vendor/npm").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pin_vendors_modules_the_packages_load_on_demand() {
        let root = temp_app("subpath");
        let (resolve_url, resolve_body) = resolved("shiki", "latest", "4.5.0");
        // Two versions of the themes are asked for (two shiki builds), one of the grammars.
        let shiki = "export const l={rust:()=>import(\"/npm/@shikijs/langs@4.5.0/rust/+esm\"),abap:()=>import(\"/npm/@shikijs/langs@4.5.0/abap/+esm\")};\
                     export const t=()=>import(\"/npm/@shikijs/themes@4.5.0/github-dark/+esm\");import\"/npm/shiki@4.4.2/core/+esm\";";
        let core = "export const t=()=>import(\"/npm/@shikijs/themes@4.4.2/github-dark/+esm\");";
        let cdn = FakeCdn::new(&[
            (&resolve_url, &resolve_body),
            ("https://cdn.jsdelivr.net/npm/shiki@4.5.0/+esm", shiki),
            ("https://cdn.jsdelivr.net/npm/shiki@4.4.2/core/+esm", core),
            (
                "https://cdn.jsdelivr.net/npm/@shikijs/langs@4.5.0/rust/+esm",
                "import r from\"/npm/@shikijs/langs@4.5.0/dist/rust.mjs/+esm\";export default r;",
            ),
            ("https://cdn.jsdelivr.net/npm/@shikijs/langs@4.5.0/dist/rust.mjs/+esm", "export default [];"),
            ("https://cdn.jsdelivr.net/npm/@shikijs/themes@4.5.0/github-dark/+esm", "export default {};"),
            ("https://cdn.jsdelivr.net/npm/@shikijs/themes@4.4.2/github-dark/+esm", "export default {};"),
        ]);
        let log = pin(&root, "shiki", false, &cdn).unwrap();
        let hint = log.iter().find(|l| l.starts_with("on demand")).expect("says what loads on demand");
        assert!(hint.contains("2 module(s) of @shikijs/langs@4.5.0 (abap, rust)"), "{log:?}");
        assert!(hint.contains("`kiln importmap pin @shikijs/langs/<module>[,<module>..]`"), "{hint}");

        // No version: the one the vendored build asks for, not the latest, so no resolve call.
        let log = pin(&root, "@shikijs/langs/rust", false, &cdn).unwrap();
        assert!(log.contains(&"create  assets/vendor/npm/@shikijs/langs@4.5.0/rust.js".to_owned()), "{log:?}");
        assert!(log.contains(&"create  assets/vendor/npm/@shikijs/langs@4.5.0/dist/rust.mjs.js".to_owned()), "{log:?}");
        assert!(
            log.iter().any(|l| l.contains("`import(\"/npm/@shikijs/langs@4.5.0/rust/+esm\")` load from the app")),
            "{log:?}"
        );
        assert!(!cdn.asked.borrow().iter().any(|u| u.contains("langs/resolved")));
        // Every version asked for, in one go.
        pin(&root, "@shikijs/themes/github-dark", false, &cdn).unwrap();
        for file in ["@shikijs/themes@4.5.0/github-dark.js", "@shikijs/themes@4.4.2/github-dark.js"] {
            assert!(root.join("assets/vendor/npm").join(file).is_file(), "{file}");
        }
        let text = fs::read_to_string(root.join(PINS)).unwrap();
        let pinned = pins(&text).unwrap();
        assert_eq!(pinned["@shikijs/langs/rust"], "vendor/npm/@shikijs/langs@4.5.0/rust.js");
        assert_eq!(pinned["@shikijs/themes/github-dark"], "vendor/npm/@shikijs/themes@4.5.0/github-dark.js");
        // abap still loads from jsDelivr.
        assert_eq!(pinned["/npm/"], "https://cdn.jsdelivr.net/npm/");
        assert!(text.ends_with(&format!("{LAZY_COMMENT}\n\"/npm/\" = \"https://cdn.jsdelivr.net/npm/\"\n")), "{text}");

        // Unpinning a module removes its files, though shiki still reaches them on demand.
        let log = unpin(&root, &["@shikijs/langs/rust".to_owned()]).unwrap();
        assert!(log.contains(&"remove  assets/vendor/npm/@shikijs/langs@4.5.0/rust.js".to_owned()), "{log:?}");
        assert!(log.contains(&"remove  assets/vendor/npm/@shikijs/langs@4.5.0/dist/rust.mjs.js".to_owned()), "{log:?}");
        assert!(root.join("assets/vendor/npm/@shikijs/themes@4.4.2/github-dark.js").is_file());
        assert!(!pins(&fs::read_to_string(root.join(PINS)).unwrap()).unwrap().contains_key("@shikijs/langs/rust"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pin_with_cdn_writes_the_url_only() {
        let root = temp_app("cdn");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join(PINS), "# mine\nsortable = \"vendor/sortable.js\"\n").unwrap();
        let (resolve_url, resolve_body) = resolved("canvas-confetti", "1.9.3", "1.9.3");
        let cdn = FakeCdn::new(&[(&resolve_url, &resolve_body)]);
        pin(&root, "canvas-confetti@1.9.3", true, &cdn).unwrap();
        let text = fs::read_to_string(root.join(PINS)).unwrap();
        assert_eq!(
            text,
            "# mine\nsortable = \"vendor/sortable.js\"\n\"canvas-confetti\" = \"https://cdn.jsdelivr.net/npm/canvas-confetti@1.9.3/+esm\"\n"
        );
        assert!(!root.join("assets/vendor").exists());
        assert!(unpin(&root, &["nope".to_owned()]).unwrap_err().contains("isn't pinned"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lists_controllers_with_their_names() {
        let root = temp_app("controllers");
        for file in ["hello_controller.js", "admin/users_controller.js", "bulk_edit_controller.js", "helpers.js"] {
            let path = root.join("assets/controllers").join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let found: Vec<String> = controllers(&root).into_iter().map(|(id, _)| id).collect();
        assert_eq!(found, ["admin--users", "bulk-edit", "hello"]);
        fs::remove_dir_all(root).unwrap();
    }
}
