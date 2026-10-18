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
//!
//! [`check_layouts_and_includes`] does the same for the templates a page pulls in: its layout
//! (rendered with the page's context) and its `{% include %}`s (rendered with the context at
//! the include), for every view that uses them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::template_scan::{Kind, Scan, scan};

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

/// A view or mail, with the templates it renders: its page, its layout (`None` for
/// `layout = none`), and through them every `{% include %}` and `{% extends %}`.
pub struct Owner {
    /// `views::pulls::Index`.
    pub name: String,
    /// `pulls/index.html.jinja`.
    pub template: String,
    /// `layouts/repo.html.jinja`.
    pub layout: Option<String>,
    pub fields: BTreeMap<String, Shape>,
}

/// What the layout and include checks read the templates from.
pub struct Templates<'a> {
    /// A template's source by name (`layouts/app.html.jinja`).
    pub source: &'a dyn Fn(&str) -> Option<String>,
    /// The top-level names minijinja reports each template reading without defining them.
    /// Only these are ever flagged.
    pub undeclared: &'a BTreeMap<String, BTreeSet<String>>,
    pub globals: &'a BTreeSet<String>,
    pub records: &'a Records,
}

/// A finding in a layout or an included template.
#[derive(Debug, PartialEq)]
pub struct TreeProblem {
    pub code: &'static str,
    /// `templates/layouts/repo.html.jinja`.
    pub file: String,
    pub line: usize,
    pub message: String,
}

enum Found {
    /// A name the context doesn't have.
    Missing(String),
    /// What `walk` said about an attribute.
    Attr(String),
}

struct Raw {
    code: &'static str,
    file: String,
    line: usize,
    needle: String,
    owner: String,
    via: String,
    found: Found,
}

struct Tree<'a> {
    templates: &'a Templates<'a>,
    scans: BTreeMap<String, Option<std::rc::Rc<Scan>>>,
    found: Vec<Raw>,
}

/// K0401/K0412 for what a page's view reaches beyond its own template: the layout renders
/// with the page's context (its fields, `content`, the page's `{% set %}`s, `flash`,
/// `current_*` and the template functions), and an included template with the context at
/// the `{% include %}` (plus the loop, `with` and `set` variables there). A layout or
/// partial shared by several views must work with each of them. `{% extends %}` is
/// followed, leaving out the parent's blocks the child replaces. Names a template guards
/// (`is defined`, `| default`) aren't flagged; neither are macro bodies, computed include
/// names, or anything minijinja doesn't report as a variable the template reads.
pub fn check_layouts_and_includes(owners: &[Owner], templates: &Templates) -> Vec<TreeProblem> {
    let mut tree = Tree { templates, scans: BTreeMap::new(), found: vec![] };
    for owner in owners {
        let Some(page) = tree.scan(&owner.template) else { continue };
        // The page's own reads are checked against the view already; follow what it pulls in.
        tree.visit(&owner.template, &owner.fields, owner, "extended by its page", false, &BTreeSet::new(), &mut vec![]);
        let Some(layout) = &owner.layout else { continue };
        let mut context = owner.fields.clone();
        context.insert("content".into(), Shape::Other);
        for (name, path) in &page.declared {
            let shape = path.as_deref().map_or(Shape::Other, |p| tree.path_shape(p, &[], &owner.fields));
            context.insert(name.clone(), shape);
        }
        tree.visit(layout, &context, owner, "as the layout", true, &BTreeSet::new(), &mut vec![]);
    }
    group(tree.found)
}

impl Tree<'_> {
    fn scan(&mut self, name: &str) -> Option<std::rc::Rc<Scan>> {
        let source = self.templates.source;
        self.scans.entry(name.to_owned()).or_insert_with(|| source(name).map(|s| std::rc::Rc::new(scan(&s)))).clone()
    }

    /// The shape of `path` (`post.comments`), its head looked up in `locals` then `context`.
    fn path_shape(&self, path: &str, locals: &[(String, Option<String>)], context: &BTreeMap<String, Shape>) -> Shape {
        let parts: Vec<&str> = path.split('.').collect();
        let head = match locals.iter().rposition(|(name, _)| name == parts[0]) {
            Some(at) => self.local_shape(&locals[..=at], context),
            None => context.get(parts[0]).cloned().unwrap_or(Shape::Other),
        };
        resolve(&head, &parts[1..])
    }

    /// The shape of the last of `locals`, from the path it was taken from.
    fn local_shape(&self, locals: &[(String, Option<String>)], context: &BTreeMap<String, Shape>) -> Shape {
        let Some(((_, path), outer)) = locals.split_last() else { return Shape::Other };
        path.as_deref().map_or(Shape::Other, |p| self.path_shape(p, outer, context))
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        &mut self,
        name: &str,
        context: &BTreeMap<String, Shape>,
        owner: &Owner,
        via: &str,
        own_reads: bool,
        replaced: &BTreeSet<String>,
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|s| s == name) {
            return;
        }
        let Some(scan) = self.scan(name) else { return };
        stack.push(name.to_owned());
        // What the template sets for itself (`{% set title = post.title %}`) shadows the context.
        let mut inner = context.clone();
        for (declared, path) in &scan.declared {
            let shape = path.as_deref().map_or(Shape::Other, |p| self.path_shape(p, &[], context));
            inner.insert(declared.clone(), shape);
        }
        if let Some(parent) = &scan.extends {
            // The parent renders with this template's blocks in place of its own.
            let mut replaced = replaced.clone();
            replaced.extend(scan.blocks.difference(&scan.super_blocks).cloned());
            self.visit(parent, &inner, owner, via, true, &replaced, stack);
        }
        let empty = BTreeSet::new();
        let undeclared = self.templates.undeclared.get(name).unwrap_or(&empty);
        let file = format!("templates/{name}");
        for item in &scan.items {
            if item.blocks.iter().any(|b| replaced.contains(b)) {
                continue;
            }
            match &item.kind {
                Kind::Read(path) if own_reads => {
                    let top = &path[0];
                    if scan.guarded.contains(top) {
                        continue;
                    }
                    let shape = if item.locals.iter().any(|(n, _)| n == top) {
                        let at = item.locals.iter().rposition(|(n, _)| n == top).unwrap_or(0);
                        self.local_shape(&item.locals[..=at], &inner)
                    } else if scan.declared.contains_key(top) {
                        inner.get(top).cloned().unwrap_or(Shape::Other)
                    } else if self.templates.globals.contains(top)
                        || top.starts_with("current_")
                        || !undeclared.contains(top)
                    {
                        continue;
                    } else if let Some(shape) = context.get(top) {
                        shape.clone()
                    } else {
                        self.found.push(Raw {
                            code: "K0401",
                            file: file.clone(),
                            line: item.line,
                            needle: top.clone(),
                            owner: owner.name.clone(),
                            via: via.to_owned(),
                            found: Found::Missing(top.clone()),
                        });
                        continue;
                    };
                    let attrs: Vec<&str> = path[1..].iter().map(String::as_str).collect();
                    if let Some(problem) = walk(top, &shape, &attrs, self.templates.records) {
                        self.found.push(Raw {
                            code: problem.code,
                            file: file.clone(),
                            line: item.line,
                            needle: problem.needle,
                            owner: owner.name.clone(),
                            via: via.to_owned(),
                            found: Found::Attr(problem.message),
                        });
                    }
                }
                Kind::Read(_) => {}
                Kind::Include(target) => {
                    let mut at_include = inner.clone();
                    for (k, (local, _)) in item.locals.iter().enumerate() {
                        at_include.insert(local.clone(), self.local_shape(&item.locals[..=k], &inner));
                    }
                    let via = format!("included from {file}:{}", item.line);
                    self.visit(target, &at_include, owner, &via, true, &BTreeSet::new(), stack);
                }
            }
        }
        stack.pop();
    }
}

/// One finding per place, naming every view it fails for.
fn group(found: Vec<Raw>) -> Vec<TreeProblem> {
    let mut groups: Vec<(Raw, Vec<String>)> = vec![];
    for raw in found {
        let same = |(g, _): &&mut (Raw, Vec<String>)| {
            g.code == raw.code && g.file == raw.file && g.line == raw.line && g.needle == raw.needle
        };
        match groups.iter_mut().find(|g| same(g)) {
            Some((_, owners)) => {
                if !owners.contains(&raw.owner) {
                    owners.push(raw.owner);
                }
            }
            None => {
                let owner = raw.owner.clone();
                groups.push((raw, vec![owner]));
            }
        }
    }
    groups
        .into_iter()
        .map(|(raw, owners)| {
            let shown = if owners.len() > 3 {
                format!("{} and {} more", owners[..3].join(", "), owners.len() - 3)
            } else {
                owners.join(", ")
            };
            let message = match raw.found {
                Found::Missing(top) => format!(
                    "reads `{top}`, which {shown} {} provide ({}); add the field, or guard the read with `{{% if {top} is defined %}}`",
                    if owners.len() == 1 { "doesn't" } else { "don't" },
                    raw.via
                ),
                Found::Attr(message) => format!("{message} (for {shown}, {})", raw.via),
            };
            TreeProblem { code: raw.code, file: raw.file, line: raw.line, message }
        })
        .collect()
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

    /// Checks `owners` against `files` (name → source), with minijinja's own reading of the
    /// names each template reads, as `templates --variables` reports them.
    fn tree(files: &[(&str, &str)], owners: &[Owner]) -> Vec<TreeProblem> {
        let mut env = minijinja::Environment::new();
        for (name, source) in files {
            env.add_template_owned(name.to_string(), source.to_string()).unwrap();
        }
        let undeclared: BTreeMap<String, BTreeSet<String>> = files
            .iter()
            .map(|(name, _)| {
                let used = env.get_template(name).unwrap().undeclared_variables(true);
                (name.to_string(), used.iter().map(|u| u.split('.').next().unwrap().to_owned()).collect())
            })
            .collect();
        let sources: BTreeMap<String, String> = files.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect();
        let source = |name: &str| sources.get(name).cloned();
        let globals: BTreeSet<String> = ["path", "flash", "loop", "super"].map(String::from).into();
        let records = records();
        check_layouts_and_includes(
            owners,
            &Templates { source: &source, undeclared: &undeclared, globals: &globals, records: &records },
        )
    }

    fn owner(name: &str, template: &str, layout: Option<&str>, field_types: &[(&str, &str)]) -> Owner {
        Owner {
            name: name.into(),
            template: template.into(),
            layout: layout.map(str::to_owned),
            fields: fields(field_types),
        }
    }

    fn places(problems: &[TreeProblem]) -> Vec<(&str, &str, usize)> {
        problems.iter().map(|p| (p.code, p.file.as_str(), p.line)).collect()
    }

    #[test]
    fn layouts_read_only_what_every_view_using_them_provides() {
        let layout = "<title>{% if title is defined %}{{ title }} · {% endif %}Repo</title>\n\
                      <nav>{{ repo_nav }}</nav>\n\
                      {% for kind, message in flash | items %}{{ kind }}: {{ message }}{% endfor %}\n\
                      <h1>{{ repo.title }}</h1>{{ content }} {{ path(\"posts::index\") }}";
        let files = [
            ("layouts/repo.html.jinja", layout),
            ("pulls/index.html.jinja", "{% set title = \"Pulls\" %}{{ repo.title }}"),
            ("issues/index.html.jinja", "{{ repo_nav }}"),
        ];
        let problems = tree(
            &files,
            &[
                owner(
                    "views::pulls::Index",
                    "pulls/index.html.jinja",
                    Some("layouts/repo.html.jinja"),
                    &[("repo", "Post")],
                ),
                owner(
                    "views::issues::Index",
                    "issues/index.html.jinja",
                    Some("layouts/repo.html.jinja"),
                    &[("repo", "Post"), ("repo_nav", "String")],
                ),
            ],
        );
        assert_eq!(places(&problems), [("K0401", "templates/layouts/repo.html.jinja", 2)], "{problems:?}");
        assert!(
            problems[0]
                .message
                .starts_with("reads `repo_nav`, which views::pulls::Index doesn't provide (as the layout)"),
            "{}",
            problems[0].message
        );
    }

    #[test]
    fn guarded_layout_reads_and_page_sets_are_fine() {
        let layout = "{% if repo_nav is defined %}{{ repo_nav }}{% endif %}{{ sidebar | default(\"\") }}\
                      {{ heading }}{{ content }}";
        let files = [
            ("layouts/app.html.jinja", layout),
            ("a.html.jinja", "{% set heading = \"A\" %}"),
            ("b.html.jinja", "no heading"),
        ];
        let problems = tree(
            &files,
            &[
                owner("views::A", "a.html.jinja", Some("layouts/app.html.jinja"), &[]),
                owner("views::B", "b.html.jinja", Some("layouts/app.html.jinja"), &[]),
                owner("views::Partial", "b.html.jinja", None, &[]),
            ],
        );
        assert_eq!(places(&problems), [("K0401", "templates/layouts/app.html.jinja", 1)], "{problems:?}");
        assert!(problems[0].message.starts_with("reads `heading`, which views::B doesn't"), "{}", problems[0].message);
    }

    #[test]
    fn includes_see_the_loop_and_with_variables_at_the_include() {
        let files = [
            (
                "posts/index.html.jinja",
                "{% for post in posts %}{% include \"posts/_post.html.jinja\" %}{% endfor %}\n\
                 {% with heading = \"Posts\" %}{% include \"_heading.html.jinja\" %}{% endwith %}",
            ),
            (
                "posts/_post.html.jinja",
                "{{ loop.index }} {{ post.title }} by {{ post.author.name }}\n{{ post.comments }}",
            ),
            ("_heading.html.jinja", "<h1>{{ heading }}</h1>"),
        ];
        let problems = tree(
            &files,
            &[owner(
                "views::posts::Index",
                "posts/index.html.jinja",
                None,
                &[("posts", "Vec<Loaded<Post, (post::author,)>>")],
            )],
        );
        assert_eq!(places(&problems), [("K0412", "templates/posts/_post.html.jinja", 2)], "{problems:?}");
        assert!(problems[0].message.contains("included from templates/posts/index.html.jinja:1"), "{problems:?}");
    }

    #[test]
    fn includes_reading_a_missing_field_are_found_through_nesting_without_looping() {
        let files = [
            ("posts/show.html.jinja", "<h1>{{ post.title }}</h1>\n{% include \"posts/_comments.html.jinja\" %}"),
            (
                "posts/_comments.html.jinja",
                "{% for c in post.comments %}\n{% include \"comments/_comment.html.jinja\" %}\n{% endfor %}\n{% include partial %}",
            ),
            (
                "comments/_comment.html.jinja",
                "{{ c.body }}\n{{ c.author.name }}\n{{ categories }}\n{% include \"posts/_comments.html.jinja\" %}",
            ),
        ];
        let problems = tree(
            &files,
            &[owner(
                "views::posts::Show",
                "posts/show.html.jinja",
                None,
                &[("post", "Loaded<Post, (post::comments,)>"), ("partial", "String")],
            )],
        );
        assert_eq!(
            places(&problems),
            [
                ("K0412", "templates/comments/_comment.html.jinja", 2),
                ("K0401", "templates/comments/_comment.html.jinja", 3)
            ],
            "{problems:?}"
        );
        assert!(
            problems[1].message.starts_with(
                "reads `categories`, which views::posts::Show doesn't provide (included from templates/posts/_comments.html.jinja:2)"
            ),
            "{}",
            problems[1].message
        );
    }

    #[test]
    fn extended_layouts_skip_the_blocks_they_replace() {
        let files = [
            (
                "layouts/base.html.jinja",
                "{% block nav %}{{ default_nav }}{% endblock %}\n{{ content }}\n{{ site_name }}",
            ),
            (
                "layouts/admin.html.jinja",
                "{% extends \"layouts/base.html.jinja\" %}{% block nav %}{{ admin_nav }}{% endblock %}",
            ),
            ("admin/index.html.jinja", "{% from \"forms.html.jinja\" import input %}{{ input(form) }}"),
            ("forms.html.jinja", "{% macro input(field, label=none) %}{{ field }}{{ label }}{{ outer }}{% endmacro %}"),
        ];
        let problems = tree(
            &files,
            &[owner(
                "views::admin::Index",
                "admin/index.html.jinja",
                Some("layouts/admin.html.jinja"),
                &[("admin_nav", "String"), ("form", "String")],
            )],
        );
        assert_eq!(places(&problems), [("K0401", "templates/layouts/base.html.jinja", 3)], "{problems:?}");
    }
}
