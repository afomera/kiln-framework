//! Rich text: formatted text written in an editor, stored as sanitized HTML.
//!
//! ```ignore
//! #[derive(Record)]
//! pub struct Post { pub body: RichText, .. }        // a TEXT column
//!
//! #[derive(Form)]
//! pub struct PostForm { pub body: RichText, .. }    // {{ field(form.body) }} renders the editor
//! ```
//!
//! ```jinja
//! {{ post.body | rich_text }}
//! ```
//!
//! The editor is Lexxy (vendored; see `vendor/README.md`), with Kiln's configuration and
//! skin in `assets/rich-text.{js,css}`. Whatever arrives in a form, from the editor or
//! straight from a client, is sanitized against an allowlist of exactly what the editor
//! produces before it becomes a `RichText`: a value of this type is always safe to render.
//! Attachments are off until Kiln has file storage.

use std::{fmt, ops::Deref, sync::OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{FromColumn, SqlType, ToValue, Value, form::FormField};

/// Sanitized rich text HTML.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct RichText {
    html: String,
    /// The same without markup: what length rules measure.
    text: String,
}

impl RichText {
    /// Sanitizes `html` (from the editor, a client or anywhere else).
    pub fn new(html: &str) -> Self {
        let clean = sanitizer().clean(html).to_string();
        Self::trusted(clean)
    }

    /// Already-sanitized HTML (read back from the database).
    fn trusted(html: String) -> Self {
        let text = plain_text(&html);
        // An editor with nothing in it still submits an empty paragraph.
        if text.is_empty() && !html.contains("<hr") {
            return Self::default();
        }
        Self { html, text }
    }

    /// Plain text with paragraphs and line breaks as text, e.g. for a text email part.
    pub fn from_plain_text(text: &str) -> Self {
        let paragraphs: String = text
            .split("\n\n")
            .filter(|p| !p.trim().is_empty())
            .map(|p| format!("<p>{}</p>", crate::view::escape_attribute(p.trim()).replace('\n', "<br>")))
            .collect();
        Self::new(&paragraphs)
    }

    pub fn as_html(&self) -> &str {
        &self.html
    }

    /// The text without markup, for search, excerpts and length checks.
    pub fn to_plain_text(&self) -> String {
        self.text.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.html.is_empty()
    }
}

impl Deref for RichText {
    type Target = str;

    fn deref(&self) -> &str {
        &self.html
    }
}

impl fmt::Debug for RichText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RichText({:?})", self.html)
    }
}

impl From<&str> for RichText {
    fn from(html: &str) -> Self {
        Self::new(html)
    }
}

impl From<String> for RichText {
    fn from(html: String) -> Self {
        Self::new(&html)
    }
}

impl Serialize for RichText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.html)
    }
}

/// Deserializing sanitizes too (JSON bodies, job payloads).
impl<'de> Deserialize<'de> for RichText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::new(&String::deserialize(deserializer)?))
    }
}

impl FormField for RichText {
    const INPUT: &'static str = "rich_text";

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        raw.map(Self::new).ok_or_else(|| "is missing".to_owned())
    }

    fn to_field(&self) -> String {
        self.html.clone()
    }
}

impl crate::form::rules::RuleTarget for RichText {
    /// `#[validate(length(..))]` counts what the reader sees, not the markup. Blank
    /// editors are empty, so `length(min = 1)` means "write something".
    fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

impl ToValue for RichText {
    fn to_value(&self) -> Value {
        Value::Text(self.html.clone())
    }
}

impl FromColumn for RichText {
    /// Stored values were sanitized on the way in; they're read back as they are.
    fn from_column(row: &sqlx::sqlite::SqliteRow, name: &str) -> Result<Self, sqlx::Error> {
        Ok(Self::trusted(String::from_column(row, name)?))
    }
}

impl SqlType for RichText {
    const SQL: &'static str = String::SQL;
}

fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Exactly what Kiln's editor produces, and nothing else.
fn sanitizer() -> &'static ammonia::Builder<'static> {
    static BUILDER: OnceLock<ammonia::Builder<'static>> = OnceLock::new();
    BUILDER.get_or_init(|| {
        use std::collections::{HashMap, HashSet};
        let tags: HashSet<&str> = [
            "p",
            "br",
            "strong",
            "em",
            "u",
            "s",
            "code",
            "sub",
            "sup",
            "mark",
            "a",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "blockquote",
            "pre",
            "ul",
            "ol",
            "li",
            "hr",
            "figure",
            "figcaption",
            "table",
            "thead",
            "tbody",
            "tr",
            "th",
            "td",
        ]
        .into();
        let mut attributes: HashMap<&str, HashSet<&str>> = HashMap::new();
        attributes.insert("a", ["href", "title"].into());
        attributes.insert("pre", ["data-language", "data-highlight-language"].into());
        attributes.insert("li", ["value"].into());
        attributes.insert("ol", ["start"].into());
        attributes.insert("mark", ["style"].into());
        attributes.insert("th", ["colspan", "rowspan"].into());
        attributes.insert("td", ["colspan", "rowspan"].into());
        let mut classes: HashMap<&str, HashSet<&str>> = HashMap::new();
        classes.insert("figure", ["lexxy-content__table-wrapper"].into());
        classes.insert("th", ["lexxy-content__table-cell--header"].into());

        let mut builder = ammonia::Builder::default();
        builder
            .tags(tags)
            .clean_content_tags(["script", "style"].into())
            .tag_attributes(attributes)
            .generic_attributes(HashSet::new())
            .allowed_classes(classes)
            .url_schemes(["http", "https", "mailto"].into())
            .link_rel(Some("noopener noreferrer nofollow"))
            .attribute_filter(|element, attribute, value| match (element, attribute) {
                // The editor's highlight palette only, never arbitrary CSS.
                ("mark", "style") => highlight_only(value).then(|| value.into()),
                _ => Some(value.into()),
            });
        builder
    })
}

/// `color: var(--highlight-3); background-color: var(--highlight-bg-1)`.
fn highlight_only(style: &str) -> bool {
    style.split(';').map(str::trim).filter(|d| !d.is_empty()).all(|declaration| {
        matches!(declaration.split_once(':'), Some((property, value))
        if matches!(property.trim(), "color" | "background-color")
            && value.trim().strip_prefix("var(--highlight-").is_some_and(|rest| {
                let rest = rest.strip_prefix("bg-").unwrap_or(rest);
                rest.strip_suffix(')').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editors_output_survives() {
        let html = r#"<h1>Top</h1><h2>Title</h2><p>a <strong>b</strong> <em>i</em> <u>u</u> <s>s</s> <code>c</code> <sub>x</sub> <sup>y</sup> <mark style="color: var(--highlight-1);">hl</mark> <a href="https://x.test" rel="noopener noreferrer nofollow">link</a></p><pre data-language="rust" data-highlight-language="rust">fn main() {}</pre><ul><li value="1">one<ul><li value="1">nested</li></ul></li></ul><blockquote><p>q</p></blockquote><hr><figure class="lexxy-content__table-wrapper"><table><tbody><tr><th class="lexxy-content__table-cell--header"><p>h</p></th><td><p>d</p></td></tr></tbody></table></figure>"#;
        assert_eq!(RichText::new(html).as_html(), html);
    }

    #[test]
    fn hostile_input_is_cleaned() {
        let dirty = r#"<p onclick="x()">hi<script>alert(1)</script><img src=x onerror=alert(1)><a href="javascript:alert(1)">j</a><mark style="background:url(x)">m</mark></p><iframe src="https://evil"></iframe><kiln-attachment url="javascript:alert(1)"></kiln-attachment><p style="position:fixed">s</p>"#;
        let clean = RichText::new(dirty);
        for bad in
            ["onclick", "script", "onerror", "javascript:", "iframe", "url(x)", "position", "kiln-attachment", "<img"]
        {
            assert!(!clean.contains(bad), "{bad} survived: {}", clean.as_html());
        }
        assert!(clean.contains("hi"));
    }

    #[test]
    fn empty_editors_are_empty() {
        assert!(RichText::new("<p><br></p>").is_empty());
        assert!(RichText::new("  ").is_empty());
        assert!(!RichText::new("<p>x</p>").is_empty());
        assert_eq!(RichText::new("<p>Hello <strong>you</strong> &amp; me</p>").to_plain_text(), "Hello you & me");
        assert_eq!(RichText::from_plain_text("One\n\nTwo <b>").as_html(), "<p>One</p><p>Two &lt;b&gt;</p>");
    }
}
