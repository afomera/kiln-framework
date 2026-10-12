use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, LitStr};

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a View cannot be generic"));
    }
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => f.named.iter().map(|f| f.ident.clone().expect("named")).collect::<Vec<_>>(),
            Fields::Unit => vec![],
            Fields::Unnamed(_) => return Err(Error::new_spanned(name, "a View must have named fields")),
        },
        _ => return Err(Error::new_spanned(name, "View can only be derived for structs")),
    };

    let mut template: Option<LitStr> = None;
    // `None`: not specified (use `layouts/app` if it exists). `Some(None)`: `layout = none`.
    let mut layout: Option<Option<LitStr>> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("view")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("template") {
                template = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("layout") {
                let value = meta.value()?;
                if value.peek(LitStr) {
                    layout = Some(Some(value.parse()?));
                } else {
                    let ident: syn::Ident = value.parse()?;
                    if ident != "none" {
                        return Err(Error::new_spanned(ident, "expected `layout = \"name\"` or `layout = none`"));
                    }
                    layout = Some(None);
                }
                Ok(())
            } else {
                Err(meta.error("unknown `view` option; expected `template = \"..\"` or `layout = ..`"))
            }
        })?;
    }
    let template =
        template.ok_or_else(|| Error::new_spanned(name, "missing `#[view(template = \"resource/page\")]`"))?;
    let file = format!("{}.html.jinja", template.value());

    // Fail at compile time if the template or layout file is missing.
    let templates = std::env::var("CARGO_MANIFEST_DIR").ok().map(|dir| std::path::Path::new(&dir).join("templates"));
    let exists = |file: &str| templates.as_ref().is_none_or(|t| t.join(file).exists());
    let layout_file = match layout {
        Some(Some(lit)) => {
            let layout_file = format!("layouts/{}.html.jinja", lit.value());
            if !exists(&layout_file) {
                return Err(Error::new(lit.span(), format!("layout `templates/{layout_file}` does not exist")));
            }
            Some(layout_file)
        }
        Some(None) => None,
        // Convention: pages use `layouts/app` when the app has one.
        None => templates
            .as_ref()
            .filter(|t| t.join("layouts/app.html.jinja").exists())
            .map(|_| "layouts/app.html.jinja".to_owned()),
    };
    if !exists(&file) {
        return Err(Error::new(template.span(), format!("template `templates/{file}` does not exist")));
    }
    let layout_tokens = match layout_file {
        Some(f) => quote! { ::core::option::Option::Some(#f) },
        None => quote! { ::core::option::Option::None },
    };

    let name_str = name.to_string();
    let count = fields.len();

    Ok(quote! {
        impl ::kiln::View for #name {
            const TEMPLATE: &'static str = #file;
            const ROOT: &'static str = ::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/templates");
            const LAYOUT: ::core::option::Option<&'static str> = #layout_tokens;
        }

        impl ::kiln::__private::serde::Serialize for #name {
            fn serialize<__KilnS: ::kiln::__private::serde::Serializer>(&self, __kiln_serializer: __KilnS) -> ::core::result::Result<__KilnS::Ok, __KilnS::Error> {
                let mut __kiln_state = __kiln_serializer.serialize_struct(#name_str, #count)?;
                #( ::kiln::__private::serde::ser::SerializeStruct::serialize_field(&mut __kiln_state, stringify!(#fields), &self.#fields)?; )*
                ::kiln::__private::serde::ser::SerializeStruct::end(__kiln_state)
            }
        }

        impl ::kiln::__private::IntoResponse for #name {
            fn into_response(self) -> ::kiln::__private::Response {
                ::kiln::view::respond(&self)
            }
        }
    })
}
