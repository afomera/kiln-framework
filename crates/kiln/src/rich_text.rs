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
//!
//! Attachments (images and files dropped into the editor) upload to `/kiln/uploads` and are
//! stored as `<kiln-attachment url=.. filename=.. ..>` elements pointing at signed
//! `/kiln/files/..` links; `| rich_text` turns them into figures, dropping any whose link
//! isn't one Kiln signed.

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
    fn from_column(row: &crate::Row, name: &str) -> Result<Self, sqlx::Error> {
        Ok(Self::trusted(String::from_column(row, name)?))
    }
}

impl SqlType for RichText {
    const SQL: &'static str = String::SQL;
    const RICH_TEXT: bool = true;
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
            "div",
            "kiln-attachment",
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
        attributes.insert(
            "kiln-attachment",
            [
                "sgid",
                "url",
                "content-type",
                "filename",
                "filesize",
                "width",
                "height",
                "caption",
                "alt",
                "presentation",
                "previewable",
            ]
            .into(),
        );
        let mut classes: HashMap<&str, HashSet<&str>> = HashMap::new();
        classes.insert("figure", ["lexxy-content__table-wrapper"].into());
        classes.insert("th", ["lexxy-content__table-cell--header"].into());
        classes.insert(
            "div",
            [
                "attachment-gallery",
                "attachment-gallery--1",
                "attachment-gallery--2",
                "attachment-gallery--3",
                "attachment-gallery--4",
                "attachment-gallery--5",
                "attachment-gallery--6",
                "attachment-gallery--7",
                "attachment-gallery--8",
                "attachment-gallery--9",
            ]
            .into(),
        );

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
                // Attachments only ever point at Kiln's own file links.
                ("kiln-attachment", "url") => value.starts_with(crate::storage::FILES_PATH).then(|| value.into()),
                ("kiln-attachment", "width" | "height" | "filesize") => {
                    value.chars().all(|c| c.is_ascii_digit()).then(|| value.into())
                }
                _ => Some(value.into()),
            });
        builder
    })
}

/// Turns stored `<kiln-attachment>` elements into the figures Lexxy's content styles expect.
/// `link` maps an attachment's stored `url` to the one to show, or `None` to drop it (a
/// link Kiln didn't sign). The input is sanitized HTML, so attribute values are escaped.
pub(crate) fn render_attachments(html: &str, link: &dyn Fn(&str) -> Option<String>) -> String {
    const OPEN: &str = "<kiln-attachment";
    const CLOSE: &str = "</kiln-attachment>";
    if !html.contains(OPEN) {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let tag = &rest[start..];
        let Some(tag_end) = tag.find('>') else { break };
        let attrs = parse_attributes(&tag[OPEN.len()..tag_end]);
        let after = &tag[tag_end + 1..];
        let consumed = after.find(CLOSE).map_or(0, |i| i + CLOSE.len());
        rest = &after[consumed..];
        let get = |name: &str| attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        let Some(url) = link(&decode_entities(get("url"))) else { continue };
        let url = crate::view::escape_attribute(&url);
        let content_type = get("content-type");
        let filename = get("filename");
        let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let ext: String = ext.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
        let caption = get("caption");
        if content_type.starts_with("image/") && content_type != "image/svg+xml" {
            let size: String = ["width", "height"]
                .iter()
                .filter(|a| !get(a).is_empty())
                .map(|a| format!(" {a}=\"{}\"", get(a)))
                .collect();
            out.push_str(&format!(
                "<figure class=\"attachment attachment--preview attachment--{ext}\"><a href=\"{url}\"><img src=\"{url}\" alt=\"{}\"{size} loading=\"lazy\"></a>",
                get("alt")
            ));
            if !caption.is_empty() {
                out.push_str(&format!("<figcaption class=\"attachment__caption\">{caption}</figcaption>"));
            }
            out.push_str("</figure>");
        } else {
            let size = get("filesize").parse::<u64>().map(human_size).unwrap_or_default();
            out.push_str(&format!(
                "<figure class=\"attachment attachment--file attachment--{ext}\"><span class=\"attachment__icon\">{ext}</span>\
                 <figcaption class=\"attachment__caption\"><a class=\"attachment__name\" href=\"{url}\">{}</a><span class=\"attachment__size\">{size}</span></figcaption></figure>",
                if caption.is_empty() { filename } else { caption }
            ));
        }
    }
    out.push_str(rest);
    out
}

/// `name="value"` pairs from a sanitized tag (values stay escaped).
fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let mut attrs = vec![];
    let mut rest = text.trim();
    while !rest.is_empty() {
        let name_end = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        if let Some(value) = rest.strip_prefix("=\"") {
            let end = value.find('"').unwrap_or(value.len());
            attrs.push((name, value[..end].to_owned()));
            rest = value.get(end + 1..).unwrap_or("").trim_start();
        } else {
            attrs.push((name, String::new()));
        }
    }
    attrs
}

fn decode_entities(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} bytes"),
    }
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
        for bad in ["onclick", "script", "onerror", "javascript:", "iframe", "url(x)", "position", "<img"] {
            assert!(!clean.contains(bad), "{bad} survived: {}", clean.as_html());
        }
        assert!(clean.contains("hi"));
    }

    #[test]
    fn attachments_keep_only_kiln_links_and_render_as_figures() {
        let stored = RichText::new(
            r#"<p>Look:</p><kiln-attachment sgid="t" url="/kiln/files/abc/sun.png" content-type="image/png" filename="sun.png" filesize="12" width="40" height="30" caption="The sun" presentation="gallery" onload="x()"></kiln-attachment><kiln-attachment url="https://evil.test/x.png" content-type="image/png"></kiln-attachment><kiln-attachment url="/kiln/files/def/notes.pdf" content-type="application/pdf" filename="notes.pdf" filesize="2048"></kiln-attachment>"#,
        );
        assert!(!stored.contains("onload") && !stored.contains("evil.test"), "{}", stored.as_html());
        let shown = render_attachments(stored.as_html(), &|url| {
            url.contains("/abc/").then(|| url.to_owned()).or_else(|| url.contains("/def/").then(|| url.to_owned()))
        });
        assert!(shown.contains(r#"<figure class="attachment attachment--preview attachment--png"><a href="/kiln/files/abc/sun.png"><img src="/kiln/files/abc/sun.png" alt="" width="40" height="30" loading="lazy"></a><figcaption class="attachment__caption">The sun</figcaption></figure>"#), "{shown}");
        assert!(shown.contains(r#"<a class="attachment__name" href="/kiln/files/def/notes.pdf">notes.pdf</a><span class="attachment__size">2 KB</span>"#), "{shown}");
        assert!(!shown.contains("kiln-attachment"), "the unsigned one is dropped: {shown}");
        let none = render_attachments(stored.as_html(), &|_| None);
        assert_eq!(none, "<p>Look:</p>");
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
