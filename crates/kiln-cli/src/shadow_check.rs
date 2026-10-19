//! K0407: names that hide a template helper. A view's fields are the template's top-level
//! variables, and a variable wins over a global of the same name, so a field called `path`
//! turns `path("posts::show", ..)` in its template (and its layout and partials) into a call
//! on a string, which fails only when the page renders ("string is not callable"). The same
//! goes for `{% set %}`, loop and macro-argument names inside one template.
//!
//! Filters live in their own namespace (`{{ x | avatar_hue }}`), so only functions, helper
//! macros and other globals count.

use std::collections::{BTreeMap, BTreeSet};

/// Globals that are set per render or by the template engine itself: a view field may
/// replace them on purpose (`flash`), or Rust won't allow the name (`loop`, `self`).
const PER_RENDER: &[&str] = &["flash", "loop", "self", "super", "caller", "varargs", "kwargs"];

/// The names a field can hide, each with what it is: every template global (from the app
/// binary's `templates --variables`), described by the app's helper listing when it's one
/// of the app's own.
pub fn helpers(globals: &BTreeSet<String>, listing: &serde_json::Value) -> BTreeMap<String, String> {
    let mut app: BTreeMap<String, String> = BTreeMap::new();
    for helper in listing.as_array().into_iter().flatten() {
        let (Some(name), Some(kind)) = (helper["name"].as_str(), helper["kind"].as_str()) else { continue };
        let file = helper["file"].as_str().unwrap_or_default();
        let at = match helper["line"].as_u64() {
            Some(line) => format!("{file}:{line}"),
            None => file.to_owned(),
        };
        let what = match kind {
            "macro" => format!("the helper macro `{name}()` ({at})"),
            "function" => format!("the app's template function `{name}()` ({at})"),
            _ => continue, // filters don't clash with variables
        };
        app.insert(name.to_owned(), what);
    }
    globals
        .iter()
        .filter(|g| !PER_RENDER.contains(&g.as_str()))
        .map(|g| {
            let what = app.remove(g).unwrap_or_else(|| format!("the built-in template global `{g}`"));
            (g.clone(), what)
        })
        .collect()
}

/// One name that hides a helper.
#[derive(Debug, PartialEq)]
pub struct Shadow {
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

/// View and mail fields named after a helper. `owner` is an entry of introspect's `views` or
/// `mail`; `kind` says which.
pub fn fields(owner: &serde_json::Value, kind: &str, helpers: &BTreeMap<String, String>) -> Vec<Shadow> {
    let name = owner["name"].as_str().unwrap_or_default();
    let file = owner["file"].as_str().unwrap_or_default();
    let template = owner["template"].as_str().unwrap_or_default();
    let mut out = vec![];
    for field in owner["fields"].as_array().into_iter().flatten() {
        let Some(field_name) = field["name"].as_str() else { continue };
        let Some(what) = helpers.get(field_name) else { continue };
        let renders = match owner["layout"].as_str() {
            Some(_) => format!("`{template}` and its layout and partials"),
            None => format!("`{template}` and its partials"),
        };
        out.push(Shadow {
            file: file.to_owned(),
            line: field["line"].as_u64().map(|l| l as usize),
            message: format!(
                "{kind} `{name}` has a field `{field_name}`, which hides {what} in {renders}: calling it there \
                 fails when the page renders (\"not callable\"); rename the field (e.g. `{}`)",
                suggestion(field_name, name)
            ),
        });
    }
    out
}

/// A name for the field that says what it holds: `post_path`, `current_path`.
fn suggestion(field: &str, owner: &str) -> String {
    let owner = crate::names::snake(owner);
    match owner.as_str() {
        "show" | "index" | "new" | "edit" | "" => format!("current_{field}"),
        _ => format!("{owner}_{field}"),
    }
}

/// `{% set %}`, `{% for %}`, `{% with %}` and macro-argument names in one template that hide
/// a helper the same template calls.
pub fn template(file: &str, source: &str, helpers: &BTreeMap<String, String>) -> Vec<Shadow> {
    let mut out = vec![];
    let mut reported = BTreeSet::new();
    for (offset, tag) in tags(source) {
        for (name, what) in bound_names(tag) {
            let Some(hidden) = helpers.get(&name) else { continue };
            if !calls(source, &name) || !reported.insert(name.clone()) {
                continue;
            }
            out.push(Shadow {
                file: file.to_owned(),
                line: Some(source[..offset].matches('\n').count() + 1),
                message: format!(
                    "{what} `{name}` hides {hidden}, which this template calls: `{name}(..)` in its scope fails \
                     when it renders (\"not callable\"); rename the variable"
                ),
            });
        }
    }
    out
}

/// The inside of every `{% .. %}` tag, with the tag's byte offset.
fn tags(source: &str) -> Vec<(usize, &str)> {
    let mut out = vec![];
    let mut at = 0;
    while let Some(start) = source[at..].find("{%").map(|i| at + i) {
        let Some(end) = source[start..].find("%}").map(|i| start + i) else { break };
        let inner = source[start + 2..end].trim_matches(|c: char| c == '-' || c == '+' || c.is_whitespace());
        out.push((start, inner));
        at = end + 2;
    }
    out
}

/// The names a tag binds, with what binds them.
fn bound_names(tag: &str) -> Vec<(String, &'static str)> {
    let (keyword, rest) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
    let rest = rest.trim();
    let targets = |text: &str| -> Vec<String> {
        text.split(',')
            .map(|t| t.trim().trim_matches(|c| c == '(' || c == ')').trim())
            .filter(|t| is_ident(t))
            .map(str::to_owned)
            .collect()
    };
    match keyword {
        // `{% set x = .. %}`, `{% set a, b = .. %}`, `{% set x %}..{% endset %}`
        "set" => {
            let lhs = rest.split_once('=').map_or(rest, |(lhs, _)| lhs);
            targets(lhs).into_iter().map(|n| (n, "`{% set %}` variable")).collect()
        }
        // `{% for a, b in .. %}`
        "for" => match rest.split_once(" in ") {
            Some((lhs, _)) => targets(lhs).into_iter().map(|n| (n, "loop variable")).collect(),
            None => vec![],
        },
        // `{% with a = .., b = .. %}`
        "with" => rest
            .split(',')
            .filter_map(|part| part.split_once('=').map(|(lhs, _)| lhs.trim()))
            .filter(|n| is_ident(n))
            .map(|n| (n.to_owned(), "`{% with %}` variable"))
            .collect(),
        // `{% macro card(title, icon=none) %}`
        "macro" => {
            let Some(args) = rest.split_once('(').and_then(|(_, args)| args.rsplit_once(')')).map(|(a, _)| a) else {
                return vec![];
            };
            args.split(',')
                .map(|a| a.split('=').next().unwrap_or_default().trim())
                .filter(|a| is_ident(a))
                .map(|a| (a.to_owned(), "macro argument"))
                .collect()
        }
        _ => vec![],
    }
}

fn is_ident(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Whether the template calls `name(..)` as a function: not `x.name(..)`, not the filter
/// `| name(..)`, and not `{% macro name(..) %}` defining it.
fn calls(source: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    source.match_indices(&needle).any(|(i, _)| {
        let before = source[..i].trim_end();
        let word_before = source[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
        !word_before && !before.ends_with('.') && !before.ends_with('|') && !before.ends_with("macro")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> BTreeMap<String, String> {
        let globals: BTreeSet<String> =
            ["path", "avatar", "empty", "range", "flash", "icon"].into_iter().map(str::to_owned).collect();
        let listing = serde_json::json!([
            { "name": "avatar", "kind": "macro", "file": "templates/helpers/ui.html.jinja", "line": null },
            { "name": "icon", "kind": "macro", "file": "templates/helpers/icons.html.jinja", "line": null },
            { "name": "avatar_hue", "kind": "filter", "file": "src/helpers/avatar.rs", "line": 4 },
            { "name": "empty", "kind": "function", "file": "src/helpers/empty.rs", "line": 9 },
        ]);
        helpers(&globals, &listing)
    }

    #[test]
    fn describes_what_each_name_is() {
        let helpers = known();
        assert_eq!(helpers["path"], "the built-in template global `path`");
        assert_eq!(helpers["avatar"], "the helper macro `avatar()` (templates/helpers/ui.html.jinja)");
        assert_eq!(helpers["empty"], "the app's template function `empty()` (src/helpers/empty.rs:9)");
        // Per-render values and filters can't be hidden by a field.
        assert!(!helpers.contains_key("flash"));
        assert!(!helpers.contains_key("avatar_hue"));
    }

    #[test]
    fn fields_named_after_helpers_are_flagged() {
        let view = serde_json::json!({
            "name": "Blob", "file": "src/views/repos/blob.rs", "template": "repos/blob", "layout": "layouts/repo.html.jinja",
            "fields": [
                { "name": "path", "type": "String", "line": 8 },
                { "name": "avatar", "type": "String", "line": 9 },
                { "name": "avatar_hue", "type": "u16", "line": 10 },
                { "name": "flash", "type": "String", "line": 11 },
                { "name": "title", "type": "String", "line": 12 },
            ],
        });
        let found = fields(&view, "view", &known());
        let lines: Vec<_> = found.iter().map(|s| (s.file.as_str(), s.line)).collect();
        assert_eq!(lines, [("src/views/repos/blob.rs", Some(8)), ("src/views/repos/blob.rs", Some(9))]);
        assert!(
            found[0]
                .message
                .contains("view `Blob` has a field `path`, which hides the built-in template global `path`")
        );
        assert!(found[0].message.contains("`repos/blob` and its layout and partials"), "{}", found[0].message);
        assert!(found[0].message.contains("e.g. `blob_path`"), "{}", found[0].message);
        assert!(found[1].message.contains("the helper macro `avatar()`"));
    }

    #[test]
    fn template_variables_hiding_a_called_helper_are_flagged() {
        let source = "\
{% macro empty(icon, title) %}{{ icon(icon) }}{% endmacro %}
{% for path in paths %}{{ path }}{% endfor %}
{%- set avatar = user.avatar_url -%}
<img src=\"{{ avatar }}\">{{ avatar(user.name) }}
{% with range = 3 %}{{ range }}{% endwith %}
{{ x | avatar_hue }}
";
        let found = template("templates/x.html.jinja", source, &known());
        let summary: Vec<_> = found.iter().map(|s| (s.line, s.message.split(" hides").next().unwrap())).collect();
        // `path` and `range` are bound but never called here, so they're harmless.
        assert_eq!(summary, [(Some(1), "macro argument `icon`"), (Some(3), "`{% set %}` variable `avatar`")]);
    }

    #[test]
    fn reads_bound_names() {
        assert_eq!(
            bound_names("set a, b = pair"),
            [("a".into(), "`{% set %}` variable"), ("b".into(), "`{% set %}` variable")]
        );
        assert_eq!(bound_names("set body"), [("body".into(), "`{% set %}` variable")]);
        assert_eq!(
            bound_names("for (k, v) in d | items"),
            [("k".into(), "loop variable"), ("v".into(), "loop variable")]
        );
        assert_eq!(bound_names("macro m(a, b=f(1, 2))").len(), 2);
        assert!(bound_names("if x").is_empty());
        assert!(calls("{{ path(\"x\") }}", "path"));
        assert!(!calls("{{ x.path() }} {{ y | path(1) }} {% macro path(a) %}", "path"));
    }
}
