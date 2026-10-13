use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Error, Ident, LitStr};

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a notification cannot be generic"));
    }
    let mut description: Option<LitStr> = None;
    let mut optional: Option<Vec<Ident>> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("notification")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("description") {
                description = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("optional") {
                let mut channels = vec![];
                meta.parse_nested_meta(|channel| {
                    let ident = channel
                        .path
                        .get_ident()
                        .cloned()
                        .ok_or_else(|| channel.error("expected inbox, mail or broadcast"))?;
                    if !["inbox", "mail", "broadcast"].contains(&ident.to_string().as_str()) {
                        return Err(Error::new_spanned(&ident, "expected inbox, mail or broadcast"));
                    }
                    channels.push(ident);
                    Ok(())
                })?;
                optional = Some(channels);
            } else {
                return Err(meta.error("expected `description = \"..\"` or `optional(mail, inbox)`"));
            }
            Ok(())
        })?;
    }
    let name_str = name.to_string();
    let description = description.map(|d| d.value()).unwrap_or_else(|| humanize(&name_str));
    let optional = optional.map(|channels| {
        let channels = channels.iter().map(|c| {
            let variant = Ident::new(&pascal(&c.to_string()), c.span());
            quote! { ::kiln::notify::Channel::#variant }
        });
        quote! { const OPTIONAL: &'static [::kiln::notify::Channel] = &[#(#channels),*]; }
    });
    Ok(quote! {
        impl ::kiln::notify::NotificationInfo for #name {
            const NAME: &'static str = #name_str;
            const DESCRIPTION: &'static str = #description;
            #optional
        }

        ::kiln::__private::inventory::submit! {
            ::kiln::notify::NotificationDef {
                name: #name_str,
                description: <#name as ::kiln::notify::NotificationInfo>::DESCRIPTION,
                to: ::kiln::notify::recipient_table::<#name>,
                optional: <#name as ::kiln::notify::NotificationInfo>::OPTIONAL,
                deliver: ::kiln::notify::deliver_notification::<#name>,
            }
        }
    })
}

/// `CommentPosted` → "Comment posted".
fn humanize(pascal_name: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal_name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn pascal(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
