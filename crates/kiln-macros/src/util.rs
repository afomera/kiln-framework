pub fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.char_indices() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `Current < User >` (tokens printed by `quote`) → `Current<User>`.
pub fn type_text(tokens: &str) -> String {
    let mut out = String::new();
    for part in tokens.split_whitespace() {
        let glue = out.ends_with(['<', '(', '[', '&', '#', '!'])
            || out.ends_with("::")
            || part.starts_with(['<', '>', ')', ']', ',', ':', '.', '!', '?'])
            // A call or index glues on; a tuple after `,`, `->` or `=` doesn't.
            || (part.starts_with(['(', '[']) && !out.ends_with([',', '>', '=']));
        if !out.is_empty() && !glue {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

/// The `///` comment on an item, lines joined with newlines and the leading space dropped.
pub fn doc_text(attrs: &[syn::Attribute]) -> String {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => {
                    let line = s.value();
                    Some(line.strip_prefix(' ').unwrap_or(&line).to_owned())
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    lines.join("\n").trim().to_owned()
}
