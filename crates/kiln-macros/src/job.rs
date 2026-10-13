use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Error, LitInt, LitStr};

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a job cannot be generic"));
    }
    let mut queue: Option<LitStr> = None;
    let mut attempts: Option<LitInt> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("job")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("queue") {
                queue = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("attempts") {
                attempts = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected `queue = \"..\"` or `attempts = N`"));
            }
            Ok(())
        })?;
    }
    let queue = queue.map(|q| quote! { const QUEUE: &'static str = #q; });
    let attempts = attempts.map(|a| quote! { const ATTEMPTS: i64 = #a; });
    let name_str = name.to_string();
    Ok(quote! {
        impl ::kiln::jobs::JobInfo for #name {
            const NAME: &'static str = #name_str;
            #queue
            #attempts
        }

        ::kiln::__private::inventory::submit! {
            ::kiln::jobs::JobDef { name: #name_str, run: ::kiln::jobs::run_job::<#name> }
        }
    })
}
