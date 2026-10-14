use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, ItemFn, LitStr};

/// `#[lint(code = "A0001")]` or `#[lint(code = "A0002", level = "warning")]` on
/// `fn rule(file: &File, report: &mut Report)`.
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let rule: ItemFn = syn::parse2(item)?;
    let mut code: Option<LitStr> = None;
    let mut level = quote!(::kiln::lint::Level::Error);
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("code") {
            code = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("level") {
            let value: LitStr = meta.value()?.parse()?;
            level = match value.value().as_str() {
                "error" => quote!(::kiln::lint::Level::Error),
                "warning" | "warn" => quote!(::kiln::lint::Level::Warning),
                _ => return Err(Error::new_spanned(value, "level is \"error\" (the default) or \"warning\"")),
            };
            Ok(())
        } else {
            Err(meta.error("expected `code = \"A0001\"` or `level = \"warning\"`"))
        }
    });
    syn::parse::Parser::parse2(parser, attr)?;
    let Some(code) = code else {
        return Err(Error::new_spanned(&rule.sig.ident, "a lint needs a code: `#[lint(code = \"A0001\")]`"));
    };
    let text = code.value();
    let mut chars = text.chars();
    let letter = chars.next();
    let valid = text.len() == 5 && letter.is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| c.is_ascii_digit());
    if !valid {
        return Err(Error::new_spanned(&code, "lint codes are a capital letter and four digits, like \"A0001\""));
    }
    if letter == Some('K') {
        return Err(Error::new_spanned(
            &code,
            "`K` codes are Kiln's own; use another letter for the app's rules, like \"A0001\"",
        ));
    }
    if rule.sig.inputs.len() != 2 || rule.sig.asyncness.is_some() || !rule.sig.generics.params.is_empty() {
        return Err(Error::new_spanned(&rule.sig, "a lint is `fn name(file: &File, report: &mut Report)`"));
    }
    let doc = crate::util::doc_text(&rule.attrs);
    let name = &rule.sig.ident;
    let name_text = name.to_string();
    Ok(quote! {
        #rule

        // Rules run from development builds; release binaries leave them (and syn) out.
        #[cfg(debug_assertions)]
        ::kiln::__private::inventory::submit! {
            ::kiln::lint::LintDef {
                code: #code,
                level: #level,
                name: #name_text,
                doc: #doc,
                file: ::core::file!(),
                line: ::core::line!(),
                check: #name,
            }
        }
    })
}
