//! Markdown (GitHub-flavored) and syntax highlighting.
//!
//! ```ignore
//! #[derive(Record)]
//! pub struct Issue { pub body: Markdown, .. }       // a TEXT column holding the source
//!
//! kiln::markdown::to_html("# Hi\n\n- [x] done");    // sanitized HTML
//! kiln::markdown::highlight(&source, "src/main.rs"); // a whole file, for a blob viewer
//! ```
//!
//! ```jinja
//! {{ issue.body | markdown }}
//! {{ blob.text | highlight(blob.path) }}
//! ```
//!
//! Markdown is parsed by comrak with the GFM extensions (tables, task lists, strikethrough,
//! autolinks, footnotes, alerts) and raw HTML allowed, then the result is sanitized with an
//! allowlist close to GitHub's: no scripts, event handlers, styles or `javascript:` URLs.
//! Element ids are prefixed `user-content-` (and in-page `#links` to match) so content
//! can't clobber the page's own ids.
//!
//! Code is highlighted on the server by syntect (Sublime Text grammars) as `<span
//! class="hl-..">` scopes, not inline colors, so the stylesheet decides the theme (light
//! and dark ship in `/kiln/markdown.css`) and no `style` attribute is ever allowed. Inputs
//! past a size cap render as plain escaped text: a 5 MB file never stalls a request.

use std::{
    fmt,
    ops::Deref,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use syntect::{
    parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

use crate::{FromColumn, SqlType, ToValue, Value, form::FormField, view::escape_attribute};

/// Markdown past this renders as escaped text with a note.
pub const MAX_MARKDOWN_BYTES: usize = 1024 * 1024;
/// Code past this, or with a line past [`MAX_HIGHLIGHT_LINE`], isn't highlighted:
/// regex grammars slow down sharply on huge files and minified one-line bundles.
pub const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;
pub const MAX_HIGHLIGHT_LINE: usize = 5_000;

/// Markdown source text, e.g. an issue body. Stored as written; render it with
/// `{{ issue.body | markdown }}` or [`Markdown::to_html`]. Forms show a textarea.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Markdown(String);

impl Markdown {
    pub fn new(source: impl Into<String>) -> Self {
        Self(source.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Sanitized HTML, as [`to_html`].
    pub fn to_html(&self) -> String {
        to_html(&self.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl Deref for Markdown {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Markdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Markdown({:?})", self.0)
    }
}

impl From<&str> for Markdown {
    fn from(source: &str) -> Self {
        Self::new(source)
    }
}

impl From<String> for Markdown {
    fn from(source: String) -> Self {
        Self(source)
    }
}

impl Serialize for Markdown {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Markdown {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(String::deserialize(deserializer)?))
    }
}

impl FormField for Markdown {
    const INPUT: &'static str = "textarea";

    /// Only trailing whitespace goes: leading indentation is a code block.
    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        raw.map(|s| Self::new(s.trim_end())).ok_or_else(|| "is missing".to_owned())
    }

    fn to_field(&self) -> String {
        self.0.clone()
    }
}

impl crate::form::rules::RuleTarget for Markdown {
    fn text(&self) -> Option<&str> {
        Some(&self.0)
    }
}

impl ToValue for Markdown {
    fn to_value(&self) -> Value {
        Value::Text(self.0.clone())
    }
}

impl FromColumn for Markdown {
    fn from_column(row: &crate::Row, name: &str) -> Result<Self, sqlx::Error> {
        String::from_column(row, name).map(Self)
    }
}

impl SqlType for Markdown {
    const SQL: &'static str = String::SQL;
}

/// GitHub-flavored markdown as sanitized HTML (without a wrapper element; the
/// `| markdown` filter adds `<div class="kiln-markdown">` and the stylesheet).
pub fn to_html(source: &str) -> String {
    if source.len() > MAX_MARKDOWN_BYTES {
        return format!(
            "<p class=\"kiln-markdown-note\">This document is too large to render ({} KB); shown as plain text.</p><pre>{}</pre>",
            source.len() / 1024,
            escape_attribute(source)
        );
    }
    let mut options = comrak::Options::default();
    let ext = &mut options.extension;
    ext.strikethrough = true;
    ext.table = true;
    ext.autolink = true;
    ext.tasklist = true;
    ext.footnotes = true;
    ext.alerts = true;
    // Ids are prefixed by the sanitizer, with the links that point at them.
    ext.header_id_prefix = Some(String::new());
    // READMEs lean on raw HTML (`<details>`, `<p align="center">`); the sanitizer decides.
    options.render.r#unsafe = true;
    options.render.tasklist_classes = true;
    let mut plugins = comrak::options::Plugins::default();
    let fenced = FencedCode { budget: AtomicUsize::new(MAX_HIGHLIGHT_BYTES) };
    plugins.render.codefence_syntax_highlighter = Some(&fenced);
    let html = comrak::markdown_to_html_with_plugins(source, &options, &plugins);
    sanitizer().clean(&html).to_string()
}

/// A whole file highlighted for display: `<pre class="kiln-code"><code>..</code></pre>`.
/// The language comes from `filename` (extension, a name like `Makefile`, or a shebang
/// line); unknown languages and oversized files are escaped plain text.
pub fn highlight(code: &str, filename: &str) -> String {
    let lang = syntax_for_file(code, filename).map(|s| s.name.as_str()).unwrap_or("Plain Text");
    let body = match lines_for(code, syntax_for_file(code, filename)) {
        Some(lines) => lines.join("\n"),
        None if too_big(code) => {
            return format!(
                "<p class=\"kiln-code-note\">Too large to highlight; shown as plain text.</p><pre class=\"kiln-code\"><code>{}</code></pre>",
                escape_attribute(code)
            );
        }
        None => escape_attribute(code),
    };
    format!("<pre class=\"kiln-code\" data-language=\"{}\"><code>{body}</code></pre>", escape_attribute(lang))
}

/// One HTML string per line, each with its spans closed, for line-numbered views:
/// `{% for line in blob.text | highlight_lines(blob.path) %}`. Oversized or unknown
/// code comes back as escaped lines.
pub fn highlight_lines(code: &str, filename: &str) -> Vec<String> {
    lines_for(code, syntax_for_file(code, filename)).unwrap_or_else(|| code.lines().map(escape_attribute).collect())
}

/// The URL of the stylesheet for markdown content and highlighted code.
pub fn stylesheet_url() -> &'static str {
    crate::assets::markdown_css_url()
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A code fence's info string (`rust`, `js`, `shell`) to a grammar.
fn syntax_for_token(token: &str) -> Option<&'static SyntaxReference> {
    let token = token.trim().to_ascii_lowercase();
    if token.is_empty() || matches!(token.as_str(), "text" | "plain" | "plaintext" | "txt") {
        return None;
    }
    syntaxes().find_syntax_by_token(alias(&token))
}

fn syntax_for_file(code: &str, filename: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
    set.find_syntax_by_extension(name)
        .or_else(|| ext.as_deref().and_then(|ext| set.find_syntax_by_extension(alias(ext))))
        .or_else(|| set.find_syntax_by_first_line(code.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text")
}

/// Names GitHub accepts that the bundled grammars don't, mapped to the closest one.
fn alias(token: &str) -> &str {
    match token {
        "shell" | "console" | "zsh" | "shellsession" => "sh",
        // No TypeScript grammar is bundled; JavaScript's covers most of it.
        "ts" | "typescript" | "tsx" | "jsx" | "mjs" | "cjs" | "javascript" => "js",
        "c++" => "cpp",
        "golang" => "go",
        "objc" | "objective-c" => "m",
        "yml" => "yaml",
        "jsonc" | "json5" => "json",
        "html+erb" => "erb",
        "patch" => "diff",
        other => other,
    }
}

fn too_big(code: &str) -> bool {
    code.len() > MAX_HIGHLIGHT_BYTES || code.lines().any(|l| l.len() > MAX_HIGHLIGHT_LINE)
}

/// Highlighted lines, each closing the spans it opened and reopening the ones still in
/// scope from the line before, so any line can be shown on its own. `None` when there's
/// no grammar, the code is too big, or the grammar fails on it.
fn lines_for(code: &str, syntax: Option<&SyntaxReference>) -> Option<Vec<String>> {
    let syntax = syntax?;
    if too_big(code) {
        return None;
    }
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    // Scope names sit behind a global lock; each is looked up once per call.
    let mut spans_for: std::collections::HashMap<Scope, String> = std::collections::HashMap::new();
    for line in LinesWithEndings::from(code) {
        let mut html = String::new();
        for scope in stack.as_slice() {
            html.push_str(spans_for.entry(*scope).or_insert_with(|| open_span(*scope)));
        }
        let ops = state.parse_line(line, set).ok()?;
        let (spans, _) = syntect::html::line_tokens_to_classed_spans(line, &ops, CLASS_STYLE, &mut stack).ok()?;
        html.push_str(&spans);
        // The newline is the only raw one in escaped text; lines are joined by the caller.
        html.retain(|c| c != '\n' && c != '\r');
        html.extend(std::iter::repeat_n("</span>", stack.len()));
        lines.push(html);
    }
    Some(lines)
}

const CLASS_STYLE: syntect::html::ClassStyle = syntect::html::ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// `source.rust` → `<span class="hl-source hl-rust">`, as syntect writes them.
fn open_span(scope: Scope) -> String {
    let classes: Vec<String> = scope.build_string().split('.').map(|atom| format!("hl-{atom}")).collect();
    format!("<span class=\"{}\">", classes.join(" "))
}

/// Highlights fenced code blocks during markdown rendering, up to [`MAX_HIGHLIGHT_BYTES`]
/// across the document so many large blocks can't add up; the rest are plain.
struct FencedCode {
    budget: AtomicUsize,
}

impl comrak::adapters::SyntaxHighlighterAdapter for FencedCode {
    fn write_highlighted(&self, output: &mut dyn fmt::Write, lang: Option<&str>, code: &str) -> fmt::Result {
        let within_budget =
            self.budget.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(code.len())).is_ok();
        let syntax = lang.and_then(syntax_for_token).filter(|_| within_budget);
        match lines_for(code, syntax) {
            Some(lines) => {
                for line in lines {
                    output.write_str(&line)?;
                    output.write_char('\n')?;
                }
                Ok(())
            }
            None => comrak::html::escape(output, code),
        }
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn fmt::Write,
        _attributes: std::collections::HashMap<&'static str, std::borrow::Cow<'_, str>>,
    ) -> fmt::Result {
        output.write_str("<pre class=\"kiln-code\">")
    }

    fn write_code_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: std::collections::HashMap<&'static str, std::borrow::Cow<'_, str>>,
    ) -> fmt::Result {
        comrak::html::write_opening_tag(output, "code", attributes)
    }
}

/// Classes rendered markdown may carry; `hl-*` on spans and `language-*` on code too.
const CLASSES: &[&str] = &[
    "anchor",
    "contains-task-list",
    "task-list-item",
    "task-list-item-checkbox",
    "footnotes",
    "footnote-ref",
    "footnote-backref",
    "kiln-code",
    "markdown-alert",
    "markdown-alert-title",
    "markdown-alert-note",
    "markdown-alert-tip",
    "markdown-alert-important",
    "markdown-alert-warning",
    "markdown-alert-caution",
];

fn allowed_class(element: &str, class: &str) -> bool {
    CLASSES.contains(&class)
        || (element == "span" && class.starts_with("hl-"))
        || (element == "code" && class.starts_with("language-"))
}

const ID_PREFIX: &str = "user-content-";

/// Roughly GitHub's allowlist for rendered markdown.
fn sanitizer() -> &'static ammonia::Builder<'static> {
    static BUILDER: OnceLock<ammonia::Builder<'static>> = OnceLock::new();
    BUILDER.get_or_init(|| {
        use std::collections::{HashMap, HashSet};
        let tags: HashSet<&str> = [
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "p",
            "br",
            "hr",
            "strong",
            "b",
            "em",
            "i",
            "del",
            "s",
            "ins",
            "sub",
            "sup",
            "mark",
            "code",
            "kbd",
            "samp",
            "var",
            "pre",
            "blockquote",
            "q",
            "ul",
            "ol",
            "li",
            "dl",
            "dt",
            "dd",
            "a",
            "img",
            "picture",
            "source",
            "table",
            "caption",
            "thead",
            "tbody",
            "tfoot",
            "tr",
            "th",
            "td",
            "details",
            "summary",
            "div",
            "span",
            "section",
            "input",
            "abbr",
            "small",
        ]
        .into();
        let mut attributes: HashMap<&str, HashSet<&str>> = HashMap::new();
        attributes.insert(
            "a",
            ["href", "class", "aria-label", "data-footnote-ref", "data-footnote-backref", "data-footnote-backref-idx"]
                .into(),
        );
        attributes.insert("img", ["src", "alt", "width", "height"].into());
        attributes.insert("source", ["srcset", "media", "type"].into());
        attributes.insert("input", ["type", "checked", "disabled", "class"].into());
        attributes.insert("th", ["align", "colspan", "rowspan"].into());
        attributes.insert("td", ["align", "colspan", "rowspan"].into());
        attributes.insert("ol", ["start"].into());
        attributes.insert("ul", ["class"].into());
        attributes.insert("li", ["class"].into());
        attributes.insert("code", ["class"].into());
        attributes.insert("pre", ["class"].into());
        attributes.insert("span", ["class"].into());
        attributes.insert("div", ["class"].into());
        attributes.insert("p", ["class"].into());
        attributes.insert("sup", ["class"].into());
        attributes.insert("section", ["class", "data-footnotes"].into());
        attributes.insert("details", ["open"].into());
        attributes.insert("abbr", ["title"].into());

        let mut builder = ammonia::Builder::default();
        builder
            .tags(tags)
            .clean_content_tags(["script", "style"].into())
            .tag_attributes(attributes)
            .generic_attributes(["id", "title", "align", "dir", "lang"].into())
            .url_schemes(["http", "https", "mailto"].into())
            .link_rel(Some("nofollow noopener noreferrer"))
            .attribute_filter(|element, attribute, value| match attribute {
                "class" => {
                    let kept: Vec<&str> = value.split_whitespace().filter(|c| allowed_class(element, c)).collect();
                    (!kept.is_empty()).then(|| kept.join(" ").into())
                }
                "id" if value.starts_with(ID_PREFIX) => Some(value.into()),
                "id" => Some(format!("{ID_PREFIX}{value}").into()),
                "href" => match value.strip_prefix('#') {
                    Some(target) if !target.is_empty() && !target.starts_with(ID_PREFIX) => {
                        Some(format!("#{ID_PREFIX}{target}").into())
                    }
                    _ => Some(value.into()),
                },
                // Task list checkboxes only, never a text field.
                "type" if element == "input" => (value == "checkbox").then(|| value.into()),
                // srcset isn't URL-checked by ammonia: every candidate must be http(s) or relative.
                "srcset" => value
                    .split(',')
                    .all(|candidate| {
                        let url = candidate.split_whitespace().next().unwrap_or("");
                        url.starts_with("https://") || url.starts_with("http://") || !url.contains(':')
                    })
                    .then(|| value.into()),
                _ => Some(value.into()),
            });
        builder
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_markdown_is_neutralized() {
        let dirty = "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[x](javascript:alert(1))\n\n\
                     <div onclick=\"x()\" style=\"position:fixed\">raw</div>\n\n<iframe src=\"https://evil.test\"></iframe>\n\n\
                     <a href=\"JaVaScRiPt:alert(1)\">j</a> <input type=\"text\" name=\"q\">\n\n\
                     <picture><source srcset=\"javascript:alert(1) 1x\"><img src=\"data:text/html,x\"></picture>\n\n\
                     <span class=\"hl-keyword evil\" id=\"csrf\">s</span>";
        let html = to_html(dirty);
        for bad in [
            "<script",
            "alert(1)</",
            "onerror",
            "onclick",
            "style",
            "javascript:",
            "JaVaScRiPt",
            "iframe",
            "type=\"text\"",
            "data:",
            "evil",
            "id=\"csrf\"",
        ] {
            assert!(!html.contains(bad), "{bad} survived: {html}");
        }
        assert!(
            html.contains("raw") && html.contains("<span class=\"hl-keyword\" id=\"user-content-csrf\">"),
            "{html}"
        );
    }

    #[test]
    fn gfm_features_render() {
        let html = to_html(
            "# Title\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n~~gone~~ https://kiln.test\n\n\
             Note[^1]\n\n[^1]: The note.\n\n> [!WARNING]\n> Careful\n\n[up](#title)\n",
        );
        for good in [
            "<h1 id=\"user-content-title\">",
            "href=\"#user-content-title\"",
            "<table>",
            "<th align=\"left\">a</th>",
            "<td align=\"right\">2</td>",
            "<input type=\"checkbox\" class=\"task-list-item-checkbox\" checked=\"\" disabled=\"\">",
            "<li class=\"task-list-item\">",
            "<del>gone</del>",
            "<a href=\"https://kiln.test\" rel=\"nofollow noopener noreferrer\">https://kiln.test</a>",
            "<section class=\"footnotes\" data-footnotes=\"\">",
            "<li id=\"user-content-fn-1\">",
            "href=\"#user-content-fn-1\"",
            "<div class=\"markdown-alert markdown-alert-warning\">",
        ] {
            assert!(html.contains(good), "missing {good}: {html}");
        }
    }

    #[test]
    fn fenced_code_is_highlighted_with_classes() {
        let html = to_html("```rust\nfn main() { let s = \"<b>\"; }\n```\n");
        assert!(html.starts_with("<pre class=\"kiln-code\"><code class=\"language-rust\">"), "{html}");
        assert!(html.contains("<span class=\"hl-source hl-rust\">"), "{html}");
        assert!(html.contains("hl-keyword"), "{html}");
        assert!(html.contains("&lt;b&gt;") && !html.contains("<b>"), "{html}");
        assert!(!html.contains("style="), "{html}");
        // Aliases GitHub accepts.
        assert!(to_html("```shell\necho hi\n```\n").contains("hl-source hl-shell"));
        assert!(to_html("```ts\nconst a = 1;\n```\n").contains("hl-source hl-js"));
    }

    #[test]
    fn unknown_languages_are_escaped_plain_text() {
        let html = to_html("```nosuchlang\n<script>x</script>\n```\n");
        assert!(html.contains("<code class=\"language-nosuchlang\">&lt;script&gt;x&lt;/script&gt;\n</code>"), "{html}");
        assert!(!html.contains("<span"), "{html}");
        assert_eq!(
            highlight("<b>hi</b>", "notes.unknownext"),
            "<pre class=\"kiln-code\" data-language=\"Plain Text\"><code>&lt;b&gt;hi&lt;/b&gt;</code></pre>"
        );
    }

    #[test]
    fn files_highlight_by_name_extension_or_shebang() {
        let rs = highlight("/* a\n b */\nfn x() {}\n", "src/main.rs");
        assert!(rs.starts_with("<pre class=\"kiln-code\" data-language=\"Rust\"><code>"), "{rs}");
        assert!(rs.contains("hl-comment"), "{rs}");
        assert!(highlight("all:\n\techo\n", "Makefile").contains("data-language=\"Makefile\""));
        assert!(highlight("#!/usr/bin/env python\nprint(1)\n", "script").contains("data-language=\"Python\""));
        // A comment spanning lines is reopened on each, so every line stands alone.
        let lines = highlight_lines("/* a\n b */\nfn x() {}\n", "main.rs");
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(line.matches("<span").count(), line.matches("</span>").count(), "{line}");
        }
        assert!(lines[1].contains("hl-comment"), "{}", lines[1]);
    }

    #[test]
    fn large_inputs_fall_back_to_plain_text() {
        let huge = "x<".repeat(MAX_MARKDOWN_BYTES);
        let html = to_html(&huge);
        assert!(html.starts_with("<p class=\"kiln-markdown-note\">") && html.contains("x&lt;x&lt;"));
        let minified = format!("var a = \"{}\";", "a".repeat(MAX_HIGHLIGHT_LINE));
        let shown = highlight(&minified, "app.js");
        assert!(shown.contains("kiln-code-note") && !shown.contains("hl-"), "{}", &shown[..200]);
        assert_eq!(highlight_lines(&minified, "app.js").len(), 1);
        // Fenced blocks share one budget per document: a block that would fit on its own
        // stays plain once an earlier one has used part of it.
        let block = "x = 1\n".repeat(MAX_HIGHLIGHT_BYTES / 6 - 1);
        let html = to_html(&format!("```py\nx = 1\ny = 2\n```\n\n```py\n{block}```\n"));
        let (first, second) = html.split_once("</pre>").unwrap();
        assert!(first.contains("hl-source hl-python") && !second.contains("<span"));
    }

    #[test]
    fn markdown_fields_keep_their_source() {
        let body = Markdown::from_field(Some("    indented code\n\n")).unwrap();
        assert_eq!(body.as_str(), "    indented code");
        assert!(body.to_html().contains("<pre class=\"kiln-code\"><code>indented code"));
        assert!(Markdown::new(" \n").is_empty());
    }
}
