//! Inflection for generated names.

/// `BlogPost` / `blog_post` → `BlogPost`.
pub fn pascal(name: &str) -> String {
    name.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect()
}

/// `BlogPost` → `blog_post`.
pub fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else if c == '-' || c == ' ' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

/// Pluralizes the last word of a snake_case name: `blog_post` → `blog_posts`.
pub fn plural(snake: &str) -> String {
    let (head, last) = match snake.rsplit_once('_') {
        Some((head, last)) => (format!("{head}_"), last),
        None => (String::new(), snake),
    };
    let irregular = [("person", "people"), ("child", "children"), ("man", "men"), ("woman", "women")];
    if let Some((_, plural)) = irregular.iter().find(|(s, _)| *s == last) {
        return format!("{head}{plural}");
    }
    let plural = if last.ends_with('y') && !last.ends_with("ay") && !last.ends_with("ey") && !last.ends_with("oy") {
        format!("{}ies", &last[..last.len() - 1])
    } else if ["s", "x", "z", "ch", "sh"].iter().any(|end| last.ends_with(end)) {
        format!("{last}es")
    } else {
        format!("{last}s")
    };
    format!("{head}{plural}")
}

/// `blog_post` → `Blog post` (for headings and messages).
pub fn human(snake: &str) -> String {
    let words = snake.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn is_snake(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflects() {
        assert_eq!(pascal("blog_post"), "BlogPost");
        assert_eq!(snake("BlogPost"), "blog_post");
        assert_eq!(plural("blog_post"), "blog_posts");
        assert_eq!(plural("category"), "categories");
        assert_eq!(plural("day"), "days");
        assert_eq!(plural("box"), "boxes");
        assert_eq!(plural("person"), "people");
        assert_eq!(human("blog_post"), "Blog post");
    }
}
