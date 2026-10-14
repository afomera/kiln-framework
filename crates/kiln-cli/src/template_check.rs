//! K0401 and K0412: what a page's template reads must be something its view provides.
//!
//! The view's fields are the template's variables, and a record in a field carries its
//! columns, its parts and only the relations its `Loaded<..>` type names. A template that
//! reads anything else fails when the page renders; these rules find it at check time:
//!
//! - K0401: a variable the view doesn't have (`{{ titel }}`), or an attribute the record
//!   doesn't have (`{{ post.word_count }}` with no column or part of that name).
//! - K0412: a relation the view didn't load (`{{ post.author.name }}` when the field is
//!   `Loaded<Post, (post::comments,)>`).
//!
//! Loops over a field or a loaded relation (`{% for c in post.comments %}`) are followed one
//! loop deep, so `c.author` is checked too.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// What a value in a template is, as far as the checks care.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A record, with the relations loaded on it (each with what's loaded below it).
    Record { name: String, loaded: BTreeMap<String, Shape> },
    /// Anything else: strings, numbers, forms, view structs. Not checked further.
    Other,
}

/// A finding before it becomes one: code, message, the text to locate it by.
#[derive(Debug, PartialEq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    pub needle: String,
}

/// The app's records: columns, parts and relations by record name.
#[derive(Default)]
pub struct Records {
    columns: BTreeMap<String, BTreeSet<String>>,
    relations: BTreeMap<String, BTreeMap<String, String>>,
}

impl Records {
    /// From `kiln introspect`'s `records` and `parts`.
    pub fn from_introspection(report: &Value) -> Self {
        let mut records = Records::default();
        for r in report["records"].as_array().into_iter().flatten() {
            let name = r["name"].as_str().unwrap_or_default().to_owned();
            let columns = r["columns"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str());
            records.columns.entry(name.clone()).or_default().extend(columns.map(str::to_owned));
            let relations = r["relations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|rel| Some((rel["name"].as_str()?.to_owned(), rel["target"].as_str()?.to_owned())));
            records.relations.entry(name).or_default().extend(relations);
        }
        for part in report["parts"].as_array().into_iter().flatten() {
            if let (Some(record), Some(name)) = (part["record"].as_str(), part["name"].as_str()) {
                records.columns.entry(record.to_owned()).or_default().insert(name.to_owned());
            }
        }
        records
    }

    fn is_record(&self, name: &str) -> bool {
        self.columns.contains_key(name)
    }
}

/// A field's type as written (`Option<Loaded<Post, (post::author,)>>`) as a shape.
pub fn shape_of(ty: &str, records: &Records) -> Shape {
    let ty = ty.trim();
    for wrapper in ["Option<", "Vec<", "Box<", "&"] {
        if let Some(inner) = ty.strip_prefix(wrapper) {
            let inner = if wrapper == "&" { inner } else { inner.strip_suffix('>').unwrap_or(inner) };
            return shape_of(inner, records);
        }
    }
    if let Some(inner) = ty.strip_prefix("Loaded<").and_then(|t| t.strip_suffix('>')) {
        let (record, relations) = split_once_top(inner, ',').unwrap_or((inner, ""));
        let record = record.trim();
        if !records.is_record(record) {
            return Shape::Other;
        }
        return Shape::Record { name: record.to_owned(), loaded: loaded(record, relations, records) };
    }
    let name = ty.rsplit("::").next().unwrap_or(ty);
    if records.is_record(name) {
        Shape::Record { name: name.to_owned(), loaded: BTreeMap::new() }
    } else {
        Shape::Other
    }
}

/// `(post::author, post::comments<(comment::author,)>)` on `record` → what's loaded.
fn loaded(record: &str, relations: &str, records: &Records) -> BTreeMap<String, Shape> {
    let relations = relations.trim().trim_start_matches('(').trim_end_matches(')');
    let mut out = BTreeMap::new();
    for item in split_top(relations, ',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (path, nested) = match item.find('<') {
            Some(i) => (&item[..i], item[i + 1..].strip_suffix('>').unwrap_or("")),
            None => (item, ""),
        };
        let name = path.rsplit("::").next().unwrap_or(path).to_owned();
        let target = records.relations.get(record).and_then(|r| r.get(&name)).cloned().unwrap_or_default();
        let shape = if records.is_record(&target) {
            Shape::Record { loaded: loaded(&target, nested, records), name: target }
        } else {
            Shape::Other
        };
        out.insert(name, shape);
    }
    out
}

/// Splits on `sep` outside `<>` and `()`.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = vec![];
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn split_once_top(text: &str, sep: char) -> Option<(&str, &str)> {
    let parts = split_top(text, sep);
    (parts.len() > 1).then(|| {
        let first = parts[0];
        (first, &text[first.len() + 1..])
    })
}

/// Checks one template against the variables its view (or mail) provides.
///
/// `used` is the template's undeclared variables, nested (`post.author.name`); `fields` the
/// view's fields with their shapes; `globals` the names every template has.
pub fn check(
    owner: &str,
    source: &str,
    used: &[String],
    fields: &BTreeMap<String, Shape>,
    globals: &BTreeSet<String>,
    records: &Records,
) -> Vec<Problem> {
    let mut problems = vec![];
    let mut seen_tops = BTreeSet::new();
    for var in used {
        let parts: Vec<&str> = var.split('.').collect();
        let top = parts[0];
        if globals.contains(top) || top.starts_with("current_") {
            continue;
        }
        let Some(shape) = fields.get(top) else {
            if seen_tops.insert(top.to_owned()) {
                let known: Vec<&str> = fields.keys().map(String::as_str).collect();
                problems.push(Problem {
                    code: "K0401",
                    message: format!(
                        "the template reads `{top}`, but `{owner}` has no field `{top}` (its fields: {})",
                        if known.is_empty() { "none".into() } else { known.join(", ") }
                    ),
                    needle: top.to_owned(),
                });
            }
            continue;
        };
        if let Some(problem) = walk(top, shape, &parts[1..], records) {
            problems.push(problem);
        }
    }
    // Loop variables over a field or a loaded relation: `{% for c in post.comments %}`.
    for (var, source_path) in loops(source) {
        let path: Vec<&str> = source_path.split('.').collect();
        let Some(Shape::Record { name, loaded }) = fields.get(path[0]).map(|s| resolve(s, &path[1..])) else {
            continue;
        };
        let element = Shape::Record { name, loaded };
        for attrs in uses(source, &var) {
            let attrs: Vec<&str> = attrs.iter().map(String::as_str).collect();
            if let Some(problem) = walk(&var, &element, &attrs, records)
                && !problems.iter().any(|p| p.needle == problem.needle)
            {
                problems.push(problem);
            }
        }
    }
    problems
}

/// The shape at `path` below `shape`, following loaded relations only.
fn resolve(shape: &Shape, path: &[&str]) -> Shape {
    let mut shape = shape.clone();
    for step in path {
        shape = match &shape {
            Shape::Record { loaded, .. } => loaded.get(*step).cloned().unwrap_or(Shape::Other),
            Shape::Other => Shape::Other,
        };
    }
    shape
}

/// Follows `attrs` from a value of `shape` named `head`; the first problem, if any.
fn walk(head: &str, shape: &Shape, attrs: &[&str], records: &Records) -> Option<Problem> {
    let mut shape = shape.clone();
    let mut path = head.to_owned();
    for attr in attrs {
        let Shape::Record { name, loaded } = &shape else { return None };
        let at = format!("{path}.{attr}");
        if let Some(next) = loaded.get(*attr) {
            shape = next.clone();
        } else if records.relations.get(name).is_some_and(|r| r.contains_key(*attr)) {
            let module = snake(name);
            return Some(Problem {
                code: "K0412",
                message: format!(
                    "the template reads `{at}`, but the view doesn't load `{attr}`; add `{module}::{attr}` to the field's `Loaded<{name}, (..)>`"
                ),
                needle: at,
            });
        } else if records.columns.get(name).is_some_and(|c| c.contains(*attr)) {
            // A column or a part: a plain value (or an attachment), not followed further.
            return None;
        } else {
            let mut known: Vec<&str> = records.columns.get(name).into_iter().flatten().map(String::as_str).collect();
            known.extend(loaded.keys().map(String::as_str));
            return Some(Problem {
                code: "K0401",
                message: format!(
                    "the template reads `{at}`, but `{name}` has no column, part or loaded relation `{attr}` (it has: {})",
                    known.join(", ")
                ),
                needle: at,
            });
        }
        path = at;
    }
    None
}

/// `{% for x in a.b %}` → `(x, "a.b")`. Simple loops only (one variable, a dotted path).
fn loops(source: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    for block in source.split("{%").skip(1) {
        let block = block.trim_start_matches(['-', '+']).trim_start();
        let Some(rest) = block.strip_prefix("for ") else { continue };
        let Some((var, rest)) = rest.split_once(" in ") else { continue };
        let var = var.trim();
        let path: String =
            rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
        if is_ident(var) && !path.is_empty() {
            out.push((var.to_owned(), path));
        }
    }
    out
}

/// Every `var.a.b` in `source`, as `[a, b]`.
fn uses(source: &str, var: &str) -> Vec<Vec<String>> {
    let mut out = vec![];
    let needle = format!("{var}.");
    let mut rest = source;
    while let Some(i) = rest.find(&needle) {
        let before = rest[..i].chars().last();
        let after = &rest[i + needle.len()..];
        rest = after;
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let path: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
        let attrs: Vec<String> = path.split('.').take_while(|a| !a.is_empty()).map(str::to_owned).collect();
        if !attrs.is_empty() {
            out.push(attrs);
        }
    }
    out
}

fn is_ident(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn records() -> Records {
        Records::from_introspection(&json!({
            "records": [
                { "name": "Post", "columns": [{ "name": "id" }, { "name": "title" }, { "name": "cover" }],
                  "relations": [{ "kind": "belongs_to", "name": "author", "target": "User" },
                                { "kind": "has_many", "name": "comments", "target": "Comment" }] },
                { "name": "Comment", "columns": [{ "name": "id" }, { "name": "body" }],
                  "relations": [{ "kind": "belongs_to", "name": "author", "target": "User" }] },
                { "name": "User", "columns": [{ "name": "id" }, { "name": "name" }], "relations": [] },
            ],
            "parts": [{ "record": "Post", "name": "reading_time" }],
        }))
    }

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, Shape> {
        let records = records();
        pairs.iter().map(|(name, ty)| (name.to_string(), shape_of(ty, &records))).collect()
    }

    fn codes(problems: &[Problem]) -> Vec<(&str, &str)> {
        problems.iter().map(|p| (p.code, p.needle.as_str())).collect()
    }

    #[test]
    fn reads_loaded_types() {
        let shape = shape_of("Option<Loaded<Post, (post::author, post::comments<(comment::author,)>)>>", &records());
        let Shape::Record { name, loaded } = shape else { panic!() };
        assert_eq!(name, "Post");
        assert!(
            matches!(&loaded["comments"], Shape::Record { name, loaded } if name == "Comment" && loaded.contains_key("author"))
        );
        assert_eq!(shape_of("FormState<PostForm>", &records()), Shape::Other);
        assert!(matches!(shape_of("Vec<Post>", &records()), Shape::Record { .. }));
    }

    #[test]
    fn finds_unknown_variables_and_unloaded_relations() {
        let globals: BTreeSet<String> = ["path"].map(String::from).into();
        let f = fields(&[("post", "Loaded<Post, (post::comments,)>"), ("can_edit", "bool")]);
        let used: Vec<String> = [
            "can_edit",
            "path",
            "current_user",
            "titel",
            "post.title",
            "post.reading_time",
            "post.author.name",
            "post.wordcount",
            "post.cover.url",
        ]
        .map(String::from)
        .into();
        let source = "{% for c in post.comments %}{{ c.body }} {{ c.author.name }}{% endfor %}";
        let problems = check("Show", source, &used, &f, &globals, &records());
        assert_eq!(
            codes(&problems),
            [("K0401", "titel"), ("K0412", "post.author"), ("K0401", "post.wordcount"), ("K0412", "c.author")],
            "{problems:?}"
        );
    }

    #[test]
    fn loops_over_loaded_relations_are_followed() {
        let f = fields(&[("posts", "Vec<Loaded<Post, (post::author,)>>")]);
        let source = "{% for post in posts %}{{ post.author.name }} {{ post.comments }}{% endfor %}";
        let problems = check("Index", source, &["posts".into()], &f, &BTreeSet::new(), &records());
        assert_eq!(codes(&problems), [("K0412", "post.comments")]);
    }
}
