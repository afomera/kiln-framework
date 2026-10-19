use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Error, Fields, Lit, LitInt, meta::ParseNestedMeta};

enum Rule {
    Length {
        min: Option<LitInt>,
        max: Option<LitInt>,
    },
    Range {
        min: Option<Lit>,
        max: Option<Lit>,
    },
    Email,
    Required,
    /// `file(max_size = "5MB", accept = "image/*")`
    File {
        max_size: Option<u64>,
        accept: Option<String>,
    },
}

/// `5MB` → bytes, checked at compile time.
fn parse_size(lit: &syn::LitStr) -> syn::Result<u64> {
    let text = lit.value().trim().to_ascii_uppercase();
    let at = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
    let (number, unit) = text.split_at(at);
    let scale: u64 = match unit.trim() {
        "" | "B" => 1,
        "KB" | "K" => 1024,
        "MB" | "M" => 1024 * 1024,
        "GB" | "G" => 1024 * 1024 * 1024,
        _ => return Err(Error::new_spanned(lit, "sizes look like `500KB`, `5MB` or `1GB`")),
    };
    let number: f64 = number.parse().map_err(|_| Error::new_spanned(lit, "sizes look like `500KB`, `5MB` or `1GB`"))?;
    Ok((number * scale as f64) as u64)
}

fn parse_rules(meta: ParseNestedMeta<'_>, rules: &mut Vec<Rule>) -> syn::Result<()> {
    if meta.path.is_ident("length") {
        let (mut min, mut max) = (None, None);
        meta.parse_nested_meta(|inner| {
            if inner.path.is_ident("min") {
                min = Some(inner.value()?.parse()?);
            } else if inner.path.is_ident("max") {
                max = Some(inner.value()?.parse()?);
            } else {
                return Err(inner.error("expected `min = ..` or `max = ..`"));
            }
            Ok(())
        })?;
        rules.push(Rule::Length { min, max });
    } else if meta.path.is_ident("range") {
        let (mut min, mut max) = (None, None);
        meta.parse_nested_meta(|inner| {
            if inner.path.is_ident("min") {
                min = Some(inner.value()?.parse()?);
            } else if inner.path.is_ident("max") {
                max = Some(inner.value()?.parse()?);
            } else {
                return Err(inner.error("expected `min = ..` or `max = ..`"));
            }
            Ok(())
        })?;
        rules.push(Rule::Range { min, max });
    } else if meta.path.is_ident("email") {
        rules.push(Rule::Email);
    } else if meta.path.is_ident("required") {
        rules.push(Rule::Required);
    } else if meta.path.is_ident("file") {
        let (mut max_size, mut accept) = (None, None);
        meta.parse_nested_meta(|inner| {
            if inner.path.is_ident("max_size") {
                max_size = Some(parse_size(&inner.value()?.parse()?)?);
            } else if inner.path.is_ident("accept") {
                accept = Some(inner.value()?.parse::<syn::LitStr>()?.value());
            } else {
                return Err(inner.error("expected `max_size = \"5MB\"` or `accept = \"image/*\"`"));
            }
            Ok(())
        })?;
        rules.push(Rule::File { max_size, accept });
    } else {
        return Err(meta.error("unknown rule; expected `length(..)`, `range(..)`, `email`, `required` or `file(..)`"));
    }
    Ok(())
}

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a form cannot be generic"));
    }
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(name, "Form can only be derived for structs"));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new_spanned(name, "a form must have named fields"));
    };

    let mut parses = Vec::new();
    let mut checks = Vec::new();
    let mut builds = Vec::new();
    let mut to_fields = Vec::new();
    let mut names = Vec::new();
    let mut specs = Vec::new();
    for field in &named.named {
        let ident = field.ident.as_ref().expect("named field");
        let ty = &field.ty;
        let field_name = ident.to_string();
        if field_name.starts_with('_') {
            return Err(Error::new_spanned(
                ident,
                "form fields can't start with `_`; those names are reserved for kiln",
            ));
        }
        let local = format_ident!("__kiln_field_{}", ident);
        let mut rules = Vec::new();
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("validate")) {
            attr.parse_nested_meta(|meta| parse_rules(meta, &mut rules))?;
        }
        let mut textarea = false;
        let mut password = false;
        let mut label = humanize(&field_name);
        let mut hint: Option<String> = None;
        let mut placeholder: Option<String> = None;
        let mut autocomplete: Option<String> = None;
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("form")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("textarea") {
                    textarea = true;
                } else if meta.path.is_ident("password") {
                    password = true;
                } else if meta.path.is_ident("label") {
                    label = meta.value()?.parse::<syn::LitStr>()?.value();
                } else if meta.path.is_ident("hint") {
                    hint = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else if meta.path.is_ident("placeholder") {
                    placeholder = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else if meta.path.is_ident("autocomplete") {
                    autocomplete = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else {
                    return Err(meta.error(
                        "expected `textarea`, `password`, `label = \"..\"`, `hint = \"..\"`, `placeholder = \"..\"` or `autocomplete = \"..\"`",
                    ));
                }
                Ok(())
            })?;
        }
        let email = rules.iter().any(|r| matches!(r, Rule::Email));
        let input = if textarea {
            quote!("textarea")
        } else if password {
            quote!("password")
        } else if email {
            quote!("email")
        } else {
            quote!(<#ty as ::kiln::form::FormField>::INPUT)
        };
        let (mut min_length, mut max_length, mut min, mut max) =
            (quote!(None), quote!(None), quote!(None), quote!(None));
        let mut required = quote!(<#ty as ::kiln::form::FormField>::REQUIRED);
        for rule in &rules {
            match rule {
                Rule::Length { min: lo, max: hi } => {
                    if let Some(lo) = lo {
                        min_length = quote!(Some(#lo));
                    }
                    if let Some(hi) = hi {
                        max_length = quote!(Some(#hi));
                    }
                }
                Rule::Range { min: lo, max: hi } => {
                    if let Some(lo) = lo {
                        min = quote!(Some((#lo) as f64));
                    }
                    if let Some(hi) = hi {
                        max = quote!(Some((#hi) as f64));
                    }
                }
                Rule::Required => required = quote!(true),
                Rule::Email | Rule::File { .. } => {}
            }
        }
        let accept = rules
            .iter()
            .find_map(|r| match r {
                Rule::File { accept: Some(a), .. } => Some(a.clone()),
                _ => None,
            })
            .map_or_else(|| quote!(None), |a| quote!(Some(#a)));
        let hint = hint.map_or_else(|| quote!(None), |h| quote!(Some(#h)));
        let placeholder = placeholder.map_or_else(|| quote!(None), |p| quote!(Some(#p)));
        let autocomplete = autocomplete.map_or_else(|| quote!(None), |a| quote!(Some(#a)));
        specs.push(quote! {
            ::kiln::form::FieldSpec {
                name: #field_name,
                label: #label,
                input: #input,
                required: #required,
                min_length: #min_length,
                max_length: #max_length,
                min: #min,
                max: #max,
                options: <#ty as ::kiln::form::FormField>::OPTIONS,
                hint: #hint,
                placeholder: #placeholder,
                autocomplete: #autocomplete,
                accept: #accept,
            }
        });
        let rule_calls = rules.iter().map(|rule| match rule {
            Rule::Length { min, max } => {
                let min = min.as_ref().map_or_else(|| quote!(None), |m| quote!(Some(#m)));
                let max = max.as_ref().map_or_else(|| quote!(None), |m| quote!(Some(#m)));
                quote! { ::kiln::form::rules::length(__kiln_value, #min, #max, #field_name, &mut __kiln_errors); }
            }
            Rule::Range { min, max } => {
                let min = min.as_ref().map_or_else(|| quote!(None), |m| quote!(Some((#m) as f64)));
                let max = max.as_ref().map_or_else(|| quote!(None), |m| quote!(Some((#m) as f64)));
                quote! { ::kiln::form::rules::range(__kiln_value, #min, #max, #field_name, &mut __kiln_errors); }
            }
            Rule::Email => quote! { ::kiln::form::rules::email(__kiln_value, #field_name, &mut __kiln_errors); },
            Rule::Required => quote! { ::kiln::form::rules::required(__kiln_value, #field_name, &mut __kiln_errors); },
            Rule::File { max_size, accept } => {
                let max = max_size.map_or_else(|| quote!(None), |m| quote!(Some(#m)));
                let accept = accept.as_ref().map_or_else(|| quote!(None), |a| quote!(Some(#a)));
                quote! { ::kiln::form::rules::file(__kiln_value, #max, #accept, #field_name, &mut __kiln_errors); }
            }
        });
        parses.push(quote! {
            let #local = __kiln_fields.parse::<#ty>(#field_name, &mut __kiln_errors);
        });
        checks.push(quote! {
            if let ::core::option::Option::Some(__kiln_value) = &#local {
                #( #rule_calls )*
            }
        });
        builds.push(quote! { #ident: #local.expect("parsed when there are no errors") });
        to_fields.push(quote! {
            __kiln_fields.set_all(#field_name, ::kiln::form::FormField::to_values(&self.#ident));
        });
        names.push(field_name);
    }

    Ok(quote! {
        impl ::kiln::form::FormInput for #name {
            const FIELDS: &'static [&'static str] = &[#( #names ),*];
            const SPECS: &'static [::kiln::form::FieldSpec] = &[#( #specs ),*];

            fn from_fields(
                __kiln_fields: &::kiln::form::Fields,
            ) -> ::core::result::Result<Self, ::kiln::form::Errors> {
                let mut __kiln_errors = ::kiln::form::Errors::default();
                #( #parses )*
                #( #checks )*
                if !__kiln_errors.is_empty() {
                    return ::core::result::Result::Err(__kiln_errors);
                }
                ::core::result::Result::Ok(Self { #( #builds, )* })
            }

            fn to_fields(&self) -> ::kiln::form::Fields {
                let mut __kiln_fields = ::kiln::form::Fields::default();
                #( #to_fields )*
                __kiln_fields
            }
        }
    })
}

/// `starts_at` → `Starts at`.
fn humanize(name: &str) -> String {
    let words = name.strip_suffix("_id").unwrap_or(name).replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Stand-in used when the derive fails, so only the real error is reported.
pub fn stub(name: &syn::Ident) -> TokenStream {
    quote! {
        impl ::kiln::form::FormInput for #name {
            const FIELDS: &'static [&'static str] = &[];
            const SPECS: &'static [::kiln::form::FieldSpec] = &[];
            fn from_fields(_: &::kiln::form::Fields) -> ::core::result::Result<Self, ::kiln::form::Errors> {
                ::core::unreachable!()
            }
            fn to_fields(&self) -> ::kiln::form::Fields {
                ::core::unreachable!()
            }
        }
    }
}
