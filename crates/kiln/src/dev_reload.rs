//! Live reload in development, like hotwire-spark: pages update as you edit.
//!
//! `{{ javascript_importmap() }}` loads `assets/dev-reload.js` in development, which listens
//! to `/kiln/dev/reload` (Server-Sent Events). That stream watches the app's `templates/`
//! and `assets/` and says what changed:
//!
//! - a stylesheet: `css` with its new fingerprinted URL, swapped in place (no reload, so
//!   scroll and form state stay);
//! - a template or other asset: `html`, and the page refreshes through Turbo, keeping its
//!   scroll position;
//! - a script: `reload`, a full reload (modules can't be swapped).
//!
//! Rust changes go through `kiln dev`, which rebuilds and restarts the server; the page
//! reconnects, sees a new boot id in `hello`, and reloads.

use std::{
    collections::BTreeMap,
    convert::Infallible,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime},
};

use axum::{
    body::{Body, Bytes},
    http::header,
    response::Response,
};

use crate::assets::Assets;

pub(crate) const PATH: &str = "/kiln/dev/reload";

/// On only in development (never in tests or production).
pub(crate) fn enabled() -> bool {
    crate::config::env() == "development"
}

/// Identifies this server process, so a page can tell a restart from a dropped connection.
fn boot_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        format!("{nanos:x}")
    })
}

type Snapshot = BTreeMap<PathBuf, SystemTime>;

fn snapshot(dirs: &[PathBuf]) -> Snapshot {
    let mut files = Snapshot::new();
    let mut stack: Vec<PathBuf> = dirs.to_vec();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if let Ok(modified) = meta.modified() {
                files.insert(path, modified);
            }
        }
    }
    files
}

/// What a set of changed files needs, most disruptive first.
fn events(changed: &[PathBuf], assets_dir: &Path, assets: &Assets) -> Vec<String> {
    let in_assets = |p: &Path| p.strip_prefix(assets_dir).ok().map(|r| r.to_string_lossy().replace('\\', "/"));
    if changed.iter().any(|p| in_assets(p).is_some() && p.extension().is_some_and(|e| e == "js")) {
        return vec!["event: reload\ndata: js\n\n".to_owned()];
    }
    let (css, other): (Vec<&PathBuf>, Vec<&PathBuf>) =
        changed.iter().partition(|p| in_assets(p).is_some() && p.extension().is_some_and(|e| e == "css"));
    if !other.is_empty() {
        return vec!["event: html\ndata: changed\n\n".to_owned()];
    }
    css.iter()
        .filter_map(|p| {
            let name = in_assets(p)?;
            // A deleted stylesheet has no URL; the page reloads instead.
            let Some(url) = assets.url(&name) else { return Some("event: html\ndata: removed\n\n".to_owned()) };
            let data = serde_json::json!({ "name": name, "url": url });
            Some(format!("event: css\ndata: {data}\n\n"))
        })
        .collect()
}

/// `/kiln/dev/reload`.
pub(crate) fn serve(assets: &Assets) -> Response {
    struct State {
        assets: Assets,
        dirs: Vec<PathBuf>,
        last: Snapshot,
        idle: u32,
    }
    async fn next(mut state: State) -> Option<(Result<Bytes, Infallible>, State)> {
        loop {
            tokio::select! {
                () = crate::jobs::shutdown_requested() => return None,
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
            let now = snapshot(&state.dirs);
            if now != state.last {
                let changed: Vec<PathBuf> = now
                    .iter()
                    .filter(|(path, time)| state.last.get(*path) != Some(time))
                    .map(|(path, _)| path.clone())
                    .chain(state.last.keys().filter(|p| !now.contains_key(*p)).cloned())
                    .collect();
                state.last = now;
                // Editors often write twice (save, then format); let it settle.
                tokio::time::sleep(Duration::from_millis(60)).await;
                state.last = snapshot(&state.dirs);
                state.idle = 0;
                let chunk: String = events(&changed, state.assets.dir(), &state.assets).concat();
                if !chunk.is_empty() {
                    return Some((Ok(Bytes::from(chunk)), state));
                }
            }
            state.idle += 1;
            if state.idle >= 60 {
                state.idle = 0;
                return Some((Ok(Bytes::from_static(b": keepalive\n\n")), state));
            }
        }
    }
    let root = assets.dir().parent().map(Path::to_path_buf).unwrap_or_default();
    let dirs = vec![root.join("templates"), assets.dir().to_path_buf()];
    let state = State { assets: assets.clone(), last: snapshot(&dirs), dirs, idle: 0 };
    let hello = format!("retry: 500\nevent: hello\ndata: {}\n\n", boot_id());
    let first = futures_util::stream::once(async move { Ok::<_, Infallible>(Bytes::from(hello)) });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(futures_util::StreamExt::chain(first, futures_util::stream::unfold(state, next))))
        .expect("valid stream response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_map_to_the_gentlest_update() {
        let dir = std::env::temp_dir().join(format!("kiln-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/app.css"), "a{}").unwrap();
        let assets = Assets::at(&dir);
        let a = dir.join("assets");
        let css = events(&[a.join("app.css")], &a, &assets);
        assert!(css[0].starts_with("event: css\n") && css[0].contains("\"name\":\"app.css\""), "{css:?}");
        assert!(
            events(&[a.join("app.css"), dir.join("templates/x.html.jinja")], &a, &assets)[0].starts_with("event: html")
        );
        assert!(events(&[a.join("application.js"), a.join("app.css")], &a, &assets)[0].starts_with("event: reload"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
