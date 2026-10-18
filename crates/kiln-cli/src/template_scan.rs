//! A light reading of a Jinja template for `kiln check`: the names each expression reads,
//! with the loop, `with` and `set` variables in scope at that point, plus the template's
//! `{% include %}`s, `{% extends %}` and blocks. It isn't a full parser: what it can't make
//! sense of, it leaves out, and the checks only flag names minijinja itself reports as the
//! template's undeclared variables, so a misreading here can hide a finding but not invent one.

use std::collections::{BTreeMap, BTreeSet};

/// What one template reads and pulls in.
#[derive(Debug, Default)]
pub struct Scan {
    pub items: Vec<Item>,
    /// `{% extends "x" %}` with a literal name.
    pub extends: Option<String>,
    /// The blocks this template defines, and those whose body calls `super()`.
    pub blocks: BTreeSet<String>,
    pub super_blocks: BTreeSet<String>,
    /// Names the template defines for itself anywhere outside a macro: `{% set %}` targets,
    /// imports and macros. With the source path when every `set` of it is a plain path.
    pub declared: BTreeMap<String, Option<String>>,
    /// Names tested with `is defined` / `is undefined` or given a `| default`: reading them
    /// when they're missing is intended.
    pub guarded: BTreeSet<String>,
}

#[derive(Debug)]
pub struct Item {
    pub kind: Kind,
    pub line: usize,
    /// Loop, `with` and `call` variables in scope, innermost last, each with the dotted path
    /// it was taken from (`for c in post.comments` → `post.comments`) when there is one.
    pub locals: Vec<(String, Option<String>)>,
    /// The blocks the item is inside, outermost first.
    pub blocks: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    /// A variable read with its attributes: `post.author.name` → `["post", "author", "name"]`.
    Read(Vec<String>),
    /// `{% include "x" %}` with a literal name.
    Include(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num,
    Punct(String),
}

/// Tokens with their byte offsets in the expression.
fn lex(text: &str) -> Vec<(usize, Tok)> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'"' || c == b'\'' {
            let start = i;
            i += 1;
            let mut value = String::new();
            while i < bytes.len() && bytes[i] != c {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                let ch = text[i..].chars().next().unwrap_or(' ');
                value.push(ch);
                i += ch.len_utf8();
            }
            i += 1;
            out.push((start, Tok::Str(value)));
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80) {
                i += 1;
            }
            out.push((start, Tok::Ident(text[start..i].to_owned())));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.') {
                i += 1;
            }
            out.push((start, Tok::Num));
        } else {
            let two = text.get(i..i + 2).unwrap_or("");
            let len = if ["==", "!=", "<=", ">=", "//", "**"].contains(&two) { 2 } else { 1 };
            out.push((i, Tok::Punct(text[i..i + len].to_owned())));
            i += len;
        }
    }
    out
}

const KEYWORDS: &[&str] =
    &["and", "or", "not", "in", "is", "if", "else", "true", "false", "none", "True", "False", "None", "recursive"];

fn is_punct(tok: Option<&(usize, Tok)>, p: &str) -> bool {
    matches!(tok, Some((_, Tok::Punct(x))) if x == p)
}

fn is_ident(tok: Option<&(usize, Tok)>, word: &str) -> bool {
    matches!(tok, Some((_, Tok::Ident(x))) if x == word)
}

/// The variable reads in `tokens`, as (offset, path), and the names they guard.
fn reads(tokens: &[(usize, Tok)]) -> (Vec<(usize, Vec<String>)>, Vec<String>) {
    let mut out = vec![];
    let mut guarded = vec![];
    // The top name of the path that ended just before token `i`, for `x is defined`, `x | default`.
    let mut last: Option<(usize, String)> = None;
    let mut i = 0;
    while i < tokens.len() {
        let (offset, tok) = &tokens[i];
        let Tok::Ident(name) = tok else {
            i += 1;
            continue;
        };
        let prev = i.checked_sub(1).map(|p| &tokens[p]);
        let prev2 = i.checked_sub(2).map(|p| &tokens[p]);
        let guard_last = |guarded: &mut Vec<String>, last: &Option<(usize, String)>, end: usize| {
            if let Some((at, top)) = last
                && *at == end
            {
                guarded.push(top.clone());
            }
        };
        if is_punct(prev, ".") {
            i += 1;
            continue;
        }
        if is_punct(prev, "|") {
            // A filter: `x | default(..)` guards `x`.
            if matches!(name.as_str(), "default" | "d") {
                guard_last(&mut guarded, &last, i - 1);
            }
            i += 1;
            continue;
        }
        if is_ident(prev, "is") || (is_ident(prev, "not") && is_ident(prev2, "is")) {
            // A test: `x is defined` guards `x`.
            if matches!(name.as_str(), "defined" | "undefined") {
                let is_at = if is_ident(prev, "is") { i - 1 } else { i - 2 };
                guard_last(&mut guarded, &last, is_at);
            }
            i += 1;
            continue;
        }
        if KEYWORDS.contains(&name.as_str()) {
            i += 1;
            continue;
        }
        if is_punct(tokens.get(i + 1), "=") {
            // A keyword argument's name: `path("x", slug=..)`.
            i += 1;
            continue;
        }
        let mut path = vec![name.clone()];
        let mut j = i + 1;
        while is_punct(tokens.get(j), ".")
            && let Some((_, Tok::Ident(attr))) = tokens.get(j + 1)
        {
            path.push(attr.clone());
            j += 2;
        }
        if path.len() > 1 && is_punct(tokens.get(j), "(") {
            // A method call (`flash.items()`): not an attribute to check.
            path.pop();
        }
        last = Some((j, name.clone()));
        out.push((*offset, path));
        i = j;
    }
    (out, guarded)
}

/// `a.b.c` and nothing else → `Some("a.b.c")`.
fn plain_path(tokens: &[(usize, Tok)]) -> Option<String> {
    let mut path = String::new();
    for (k, (_, tok)) in tokens.iter().enumerate() {
        match (k % 2, tok) {
            (0, Tok::Ident(name)) if !KEYWORDS.contains(&name.as_str()) => path.push_str(name),
            (1, Tok::Punct(p)) if p == "." => path.push('.'),
            _ => return None,
        }
    }
    (!path.is_empty() && !path.ends_with('.')).then_some(path)
}

/// Splits on top-level commas.
fn split_commas(tokens: &[(usize, Tok)]) -> Vec<&[(usize, Tok)]> {
    let mut parts = vec![];
    let (mut depth, mut start) = (0i32, 0);
    for (k, (_, tok)) in tokens.iter().enumerate() {
        if let Tok::Punct(p) = tok {
            match p.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => {
                    parts.push(&tokens[start..k]);
                    start = k + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&tokens[start..]);
    parts
}

fn idents(tokens: &[(usize, Tok)]) -> Vec<String> {
    tokens
        .iter()
        .filter_map(|(_, t)| match t {
            Tok::Ident(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

struct Frame {
    kind: &'static str,
    locals: Vec<(String, Option<String>)>,
    block: Option<String>,
}

struct Scanner<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    frames: Vec<Frame>,
    scan: Scan,
    /// `set` targets seen with a path, and with something else.
    set_paths: BTreeMap<String, BTreeSet<Option<String>>>,
}

impl Scanner<'_> {
    fn line(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= offset)
    }

    fn in_macro(&self) -> bool {
        self.frames.iter().any(|f| f.kind == "macro")
    }

    fn push_item(&mut self, kind: Kind, offset: usize) {
        if self.in_macro() {
            return;
        }
        let locals = self.frames.iter().flat_map(|f| f.locals.iter().cloned()).collect();
        let blocks = self.frames.iter().filter_map(|f| f.block.clone()).collect();
        let line = self.line(offset);
        self.scan.items.push(Item { kind, line, locals, blocks });
    }

    /// Records the reads in `tokens`; `base` is the tokens' offset in the source.
    fn read(&mut self, tokens: &[(usize, Tok)], base: usize) {
        let (found, guarded) = reads(tokens);
        if self.in_macro() {
            return;
        }
        self.scan.guarded.extend(guarded);
        for (offset, path) in found {
            if path == ["super"]
                && let Some(block) = self.frames.iter().rev().find_map(|f| f.block.clone())
            {
                self.scan.super_blocks.insert(block);
            }
            self.push_item(Kind::Read(path), base + offset);
        }
    }

    fn declare(&mut self, name: String, path: Option<String>) {
        if !self.in_macro() {
            self.set_paths.entry(name).or_default().insert(path);
        }
    }

    fn pop(&mut self, kind: &str) {
        if let Some(at) = self.frames.iter().rposition(|f| f.kind == kind) {
            self.frames.truncate(at);
        }
    }

    fn push(&mut self, kind: &'static str, locals: Vec<(String, Option<String>)>, block: Option<String>) {
        self.frames.push(Frame { kind, locals, block });
    }

    fn tag(&mut self, text: &str, base: usize) {
        let tokens = lex(text);
        let Some((_, Tok::Ident(keyword))) = tokens.first() else { return };
        let rest = &tokens[1..];
        match keyword.as_str() {
            "if" | "elif" | "do" => self.read(rest, base),
            "for" => {
                let mut depth = 0;
                let Some(in_at) = rest.iter().position(|(_, t)| {
                    match t {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        _ => {}
                    }
                    depth == 0 && *t == Tok::Ident("in".into())
                }) else {
                    return;
                };
                let targets = idents(&rest[..in_at]);
                let after = &rest[in_at + 1..];
                let end = after
                    .iter()
                    .position(|(_, t)| matches!(t, Tok::Ident(w) if w == "if" || w == "recursive"))
                    .unwrap_or(after.len());
                let (iter, filter) = after.split_at(end);
                self.read(iter, base);
                let path = if targets.len() == 1 { plain_path(iter) } else { None };
                let locals = targets.into_iter().map(|t| (t, path.clone())).collect();
                self.push("for", locals, None);
                self.read(filter, base);
            }
            "with" => {
                let mut locals = vec![];
                for part in split_commas(rest) {
                    if let [(_, Tok::Ident(name)), (_, Tok::Punct(eq)), expr @ ..] = part
                        && eq == "="
                    {
                        self.read(expr, base);
                        locals.push((name.clone(), plain_path(expr)));
                    }
                }
                self.push("with", locals, None);
            }
            "set" => {
                let eq = rest.iter().position(|(_, t)| *t == Tok::Punct("=".into()));
                let (targets, expr) = match eq {
                    Some(at) => (&rest[..at], Some(&rest[at + 1..])),
                    None => (rest, None),
                };
                if targets.iter().any(|(_, t)| *t == Tok::Punct(".".into())) {
                    // `set ns.count = ..`: assigns to an attribute, declares nothing.
                    if let Some(expr) = expr {
                        self.read(expr, base);
                    }
                    return;
                }
                // Block set: `{% set nav %}..{% endset %}`, maybe with a filter.
                let targets = match expr {
                    Some(_) => targets,
                    None => {
                        let end = targets.iter().position(|(_, t)| *t == Tok::Punct("|".into()));
                        &targets[..end.unwrap_or(targets.len())]
                    }
                };
                let names = idents(targets);
                let path = match (expr, names.len()) {
                    (Some(expr), 1) => plain_path(expr),
                    _ => None,
                };
                if let Some(expr) = expr {
                    self.read(expr, base);
                }
                for name in names {
                    self.declare(name, path.clone());
                }
                if expr.is_none() {
                    self.push("set", vec![], None);
                }
            }
            "macro" => {
                if let Some((_, Tok::Ident(name))) = rest.first() {
                    let name = name.clone();
                    self.declare(name, None);
                }
                self.push("macro", vec![], None);
            }
            "call" => {
                let (params, expr) = if is_punct(rest.first(), "(") {
                    let close = rest.iter().position(|(_, t)| *t == Tok::Punct(")".into())).unwrap_or(rest.len());
                    let params: Vec<String> = split_commas(&rest[1..close])
                        .into_iter()
                        .filter_map(|p| match p.first() {
                            Some((_, Tok::Ident(name))) => Some(name.clone()),
                            _ => None,
                        })
                        .collect();
                    (params, rest.get(close + 1..).unwrap_or_default())
                } else {
                    (vec![], rest)
                };
                self.read(expr, base);
                self.push("call", params.into_iter().map(|p| (p, None)).collect(), None);
            }
            "filter" => self.push("filter", vec![], None),
            "block" => {
                if let Some((_, Tok::Ident(name))) = rest.first() {
                    self.scan.blocks.insert(name.clone());
                    let name = name.clone();
                    self.push("block", vec![], Some(name));
                }
            }
            "extends" => {
                if let [(_, Tok::Str(name))] = rest {
                    self.scan.extends = Some(name.clone());
                } else {
                    self.read(rest, base);
                }
            }
            "include" => {
                let options = ["ignore", "missing", "with", "without", "context"];
                match rest {
                    [(offset, Tok::Str(name)), tail @ ..]
                        if tail.iter().all(|(_, t)| matches!(t, Tok::Ident(w) if options.contains(&w.as_str()))) =>
                    {
                        self.push_item(Kind::Include(name.clone()), base + offset);
                    }
                    // A computed name: not followed.
                    _ => self.read(rest, base),
                }
            }
            "import" => {
                if let Some(at) = rest.iter().position(|(_, t)| *t == Tok::Ident("as".into()))
                    && let Some((_, Tok::Ident(name))) = rest.get(at + 1)
                {
                    let name = name.clone();
                    self.declare(name, None);
                }
            }
            "from" => {
                let Some(at) = rest.iter().position(|(_, t)| *t == Tok::Ident("import".into())) else { return };
                for part in split_commas(&rest[at + 1..]) {
                    let names = idents(part);
                    let name = match names.as_slice() {
                        [_, as_, alias, ..] if as_ == "as" => alias.clone(),
                        [name, ..] if name != "with" && name != "without" => name.clone(),
                        _ => continue,
                    };
                    self.declare(name, None);
                }
            }
            "endfor" => self.pop("for"),
            "endwith" => self.pop("with"),
            "endset" => self.pop("set"),
            "endmacro" => self.pop("macro"),
            "endcall" => self.pop("call"),
            "endfilter" => self.pop("filter"),
            "endblock" => self.pop("block"),
            _ => {}
        }
    }
}

/// The end of a `{{ .. }}` or `{% .. %}` starting at `from`, skipping string literals.
fn tag_end(source: &str, from: usize, close: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = from;
    let mut quote = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if source[i..].starts_with(close) => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

pub fn scan(source: &str) -> Scan {
    let mut line_starts = vec![0];
    line_starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
    let mut s = Scanner { source, line_starts, frames: vec![], scan: Scan::default(), set_paths: BTreeMap::new() };
    let mut pos = 0;
    while let Some(found) = s.source[pos..].find('{') {
        let at = pos + found;
        let open = s.source.get(at..at + 2).unwrap_or("");
        let close = match open {
            "{{" => "}}",
            "{%" => "%}",
            "{#" => "#}",
            _ => {
                pos = at + 1;
                continue;
            }
        };
        let start = at + 2;
        let end = if close == "#}" {
            s.source[start..].find("#}").map(|i| start + i)
        } else {
            tag_end(s.source, start, close)
        };
        let Some(end) = end else { break };
        pos = end + 2;
        if close == "#}" {
            continue;
        }
        let inner = &s.source[start..end];
        let trimmed = inner.trim_start_matches(['-', '+', '~']);
        let base = start + (inner.len() - trimmed.len());
        let trimmed = trimmed.trim_end_matches(['-', '+', '~']);
        if close == "}}" {
            s.read(&lex(trimmed), base);
            continue;
        }
        if trimmed.trim() == "raw" {
            // Skip to `{% endraw %}`.
            let Some(raw_end) = s.source[pos..].find("endraw") else { break };
            let after = pos + raw_end;
            pos = s.source[after..].find("%}").map_or(s.source.len(), |i| after + i + 2);
            continue;
        }
        s.tag(trimmed, base);
    }
    let mut scan = s.scan;
    for (name, paths) in s.set_paths {
        let path = if paths.len() == 1 { paths.into_iter().next().flatten() } else { None };
        scan.declared.insert(name, path);
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_paths(scan: &Scan) -> Vec<String> {
        scan.items
            .iter()
            .filter_map(|i| match &i.kind {
                Kind::Read(path) => Some(path.join(".")),
                Kind::Include(_) => None,
            })
            .collect()
    }

    #[test]
    fn reads_names_not_filters_tests_or_keyword_arguments() {
        let scan = scan(
            "{{ post.title | truncate(length=10) }} {{ path(\"posts::show\", slug=post.slug) }}\n\
             {% if title is defined and not draft %}{{ flash.items() }}{% endif %} {{ name | default(\"x\") }}\n\
             {# {{ ignored }} #}{{ \"}}\" ~ other }}{% raw %}{{ raw_text }}{% endraw %}",
        );
        assert_eq!(
            read_paths(&scan),
            ["post.title", "path", "post.slug", "title", "draft", "flash", "name", "other"],
            "{scan:?}"
        );
        assert_eq!(scan.guarded, ["name", "title"].map(String::from).into());
        assert_eq!(scan.items[3].line, 2);
    }

    #[test]
    fn tracks_loop_with_and_set_variables() {
        let scan = scan(
            "{% set title = post.title %}{% for c in post.comments if c.visible %}\
             {% include \"comments/_comment.html.jinja\" %}{% endfor %}\
             {% with form = comment_form, slug = post.slug %}{% include \"_form.html.jinja\" ignore missing %}{% endwith %}\
             {% include partial_name %}{% from \"forms.html.jinja\" import input as text_input %}\
             {% macro m(a) %}{{ a }}{{ outer }}{% endmacro %}",
        );
        let includes: Vec<_> = scan
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                Kind::Include(name) => Some((name.as_str(), i.locals.clone())),
                Kind::Read(_) => None,
            })
            .collect();
        assert_eq!(includes[0], ("comments/_comment.html.jinja", vec![("c".into(), Some("post.comments".into()))]));
        assert_eq!(
            includes[1],
            (
                "_form.html.jinja",
                vec![("form".into(), Some("comment_form".into())), ("slug".into(), Some("post.slug".into()))]
            )
        );
        assert_eq!(includes.len(), 2);
        assert!(read_paths(&scan).contains(&"partial_name".to_owned()));
        assert!(!read_paths(&scan).contains(&"outer".to_owned()), "macro bodies aren't read");
        assert_eq!(scan.declared["title"], Some("post.title".into()));
        assert!(scan.declared.contains_key("text_input") && scan.declared.contains_key("m"));
    }

    /// Every template Kiln generates or ships in the example scans, and every top-level name
    /// minijinja says one reads, the scan sees read (or defines) too.
    #[test]
    fn scans_every_template_in_the_repository() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "jinja") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = vec![];
        walk(&root.join("templates"), &mut files);
        walk(&root.join("../../examples/blog/templates"), &mut files);
        assert!(files.len() > 40);
        let env = minijinja::Environment::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            let scan = scan(&source);
            let template = env.template_from_str(&source).unwrap();
            let mut seen: BTreeSet<String> = scan.declared.keys().cloned().collect();
            for item in &scan.items {
                if let Kind::Read(path) = &item.kind {
                    seen.insert(path[0].clone());
                }
            }
            for name in template.undeclared_variables(false) {
                assert!(seen.contains(&name), "{}: the scan misses `{name}`", file.display());
            }
        }
    }

    #[test]
    fn blocks_and_extends() {
        let scan = scan("{% extends \"layouts/app.html.jinja\" %}{% block nav %}{{ super() }}{{ tabs }}{% endblock %}");
        assert_eq!(scan.extends.as_deref(), Some("layouts/app.html.jinja"));
        assert!(scan.blocks.contains("nav") && scan.super_blocks.contains("nav"));
        assert_eq!(scan.items.last().unwrap().blocks, ["nav"]);
    }
}
