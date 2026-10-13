use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, LitStr};

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a Mail cannot be generic"));
    }
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(name, "Mail can only be derived for structs"));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(name, "a Mail must have named fields"));
    };
    if !fields.named.iter().any(|f| f.ident.as_ref().is_some_and(|i| i == "to")) {
        return Err(Error::new_spanned(name, "a Mail needs a `pub to: String` field: the recipient's address"));
    }
    let mut template: Option<LitStr> = None;
    let mut subject: Option<LitStr> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("mail")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("template") {
                template = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("subject") {
                subject = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected `template = \"mail/..\"` or `subject = \"..\"`"));
            }
            Ok(())
        })?;
    }
    let template =
        template.ok_or_else(|| Error::new_spanned(name, "missing `#[mail(template = \"mail/name\", ..)]`"))?;
    let subject = subject.ok_or_else(|| {
        Error::new_spanned(name, "missing `#[mail(subject = \"..\")]` (a template string: `\"Hi {{ user.name }}\"`)")
    })?;
    let html = format!("{}.html.jinja", template.value());
    let text = format!("{}.txt.jinja", template.value());

    let templates = std::env::var("CARGO_MANIFEST_DIR").ok().map(|dir| std::path::Path::new(&dir).join("templates"));
    let exists = |file: &str| templates.as_ref().is_none_or(|t| t.join(file).exists());
    if !exists(&html) {
        return Err(Error::new(template.span(), format!("template `templates/{html}` does not exist")));
    }
    // Optional parts: present only when the files exist.
    let present = |file: &str| templates.as_ref().is_some_and(|t| t.join(file).exists());
    let text =
        if present(&text) { quote!(::core::option::Option::Some(#text)) } else { quote!(::core::option::Option::None) };
    let layout = if present("layouts/mail.html.jinja") {
        quote!(::core::option::Option::Some("layouts/mail.html.jinja"))
    } else {
        quote!(::core::option::Option::None)
    };
    Ok(quote! {
        impl ::kiln::mail::Mail for #name {
            const ROOT: &'static str = ::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/templates");
            const TEMPLATE: &'static str = #html;
            const TEXT_TEMPLATE: ::core::option::Option<&'static str> = #text;
            const LAYOUT: ::core::option::Option<&'static str> = #layout;
            const SUBJECT: &'static str = #subject;

            fn to(&self) -> &str {
                &self.to
            }
        }
    })
}
