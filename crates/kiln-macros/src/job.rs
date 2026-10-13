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
    let mut unique_for: Option<i64> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("job")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("queue") {
                queue = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("attempts") {
                attempts = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("unique_for") {
                let lit: LitStr = meta.value()?.parse()?;
                unique_for = Some(seconds(&lit.value()).ok_or_else(|| {
                    Error::new(lit.span(), "expected a duration like \"30s\", \"10m\", \"2h\" or \"1d\"")
                })?);
            } else {
                return Err(meta.error("expected `queue = \"..\"`, `attempts = N` or `unique_for = \"10m\"`"));
            }
            Ok(())
        })?;
    }
    let queue = queue.map(|q| quote! { const QUEUE: &'static str = #q; });
    let attempts = attempts.map(|a| quote! { const ATTEMPTS: i64 = #a; });
    let unique_for = unique_for
        .map(|s| quote! { const UNIQUE_FOR: ::core::option::Option<i64> = ::core::option::Option::Some(#s); });
    let name_str = name.to_string();
    Ok(quote! {
        impl ::kiln::jobs::JobInfo for #name {
            const NAME: &'static str = #name_str;
            #queue
            #attempts
            #unique_for
        }

        ::kiln::__private::inventory::submit! {
            ::kiln::jobs::JobDef { name: #name_str, run: ::kiln::jobs::run_job::<#name> }
        }
    })
}

/// "30s", "10m", "2h", "1d" in seconds.
fn seconds(text: &str) -> Option<i64> {
    let text = text.trim();
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = number.parse().ok()?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    Some(n * scale).filter(|s| *s > 0)
}
