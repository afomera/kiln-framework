use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, ItemFn, LitStr};

/// `#[template_filter]` / `#[template_function]` on a plain fn, optionally
/// `(name = "other")`: registers it with every template.
pub fn expand(attr: TokenStream, item: TokenStream, filter: bool) -> syn::Result<TokenStream> {
    let helper: ItemFn = syn::parse2(item)?;
    let mut name: Option<LitStr> = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            name = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `name = \"..\"`"))
        }
    });
    syn::parse::Parser::parse2(parser, attr)?;
    let what = if filter { "template filter" } else { "template function" };
    if helper.sig.asyncness.is_some() {
        return Err(Error::new_spanned(
            helper.sig.fn_token,
            format!("a {what} can't be async; templates render synchronously"),
        ));
    }
    if !helper.sig.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &helper.sig.generics,
            format!("a {what} can't be generic; give its arguments concrete types"),
        ));
    }
    let ident = &helper.sig.ident;
    let name = match name {
        Some(name) => {
            let text = name.value();
            let valid = text.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                return Err(Error::new_spanned(&name, "template names are identifiers, like \"avatar_hue\""));
            }
            text
        }
        None => ident.to_string().trim_start_matches("r#").to_owned(),
    };
    let doc = crate::util::doc_text(&helper.attrs);
    let (kind, add) = if filter {
        (quote!(::kiln::view::HelperKind::Filter), quote!(add_filter))
    } else {
        (quote!(::kiln::view::HelperKind::Function), quote!(add_function))
    };
    Ok(quote! {
        #helper

        ::kiln::__private::inventory::submit! {
            ::kiln::view::TemplateHelper {
                name: #name,
                kind: #kind,
                doc: #doc,
                file: ::core::file!(),
                line: ::core::line!(),
                register: |env| env.#add(#name, #ident),
            }
        }
    })
}
