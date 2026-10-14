use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Error, FnArg, GenericArgument, Ident, ItemFn, LitStr, Pat, PathArguments, Type,
    parse::{Parse, ParseStream},
};

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

struct Args {
    method: Ident,
    path: LitStr,
    pipeline: String,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let method: Ident = input.parse()?;
        if !METHODS.contains(&method.to_string().as_str()) {
            return Err(Error::new_spanned(&method, format!("expected an HTTP method: {}", METHODS.join(", "))));
        }
        let path: LitStr = input.parse()?;
        let mut pipeline = "browser".to_owned();
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: Ident = input.parse()?;
            match (key.to_string().as_str(), value.to_string().as_str()) {
                ("pipeline", "Browser") => pipeline = "browser".into(),
                ("pipeline", "Api") => pipeline = "api".into(),
                ("pipeline", _) => return Err(Error::new_spanned(value, "expected `Browser` (the default) or `Api`")),
                _ => return Err(Error::new_spanned(key, "unknown option; expected `pipeline = Browser | Api`")),
            }
        }
        Ok(Self { method, path, pipeline })
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    Literal(String),
    /// `{slug}`, or with `catch_all` a `{*path}` that takes the remaining segments.
    Capture {
        name: String,
        catch_all: bool,
    },
}

fn parse_path(lit: &LitStr) -> syn::Result<Vec<Segment>> {
    let path = lit.value();
    if !path.starts_with('/') {
        return Err(Error::new(lit.span(), "route paths start with `/`"));
    }
    let mut segments = Vec::new();
    let mut rest = path.as_str();
    while let Some(start) = rest.find('{') {
        segments.push(Segment::Literal(rest[..start].to_owned()));
        let end = rest[start..].find('}').ok_or_else(|| Error::new(lit.span(), "unclosed `{` in route"))? + start;
        let capture = &rest[start + 1..end];
        let (name, catch_all) = match capture.strip_prefix('*') {
            Some(name) => (name, true),
            None => (capture, false),
        };
        if name.is_empty() || !name.chars().all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit()) {
            let fix = if catch_all { "{*snake_case_name}" } else { "{snake_case_name}" };
            return Err(Error::new(lit.span(), format!("`{{{capture}}}` is not a valid capture; use `{fix}`")));
        }
        let whole_segment = rest[..start].ends_with('/');
        rest = &rest[end + 1..];
        if catch_all && (!rest.is_empty() || !whole_segment) {
            return Err(Error::new(
                lit.span(),
                format!(
                    "`{{{capture}}}` takes the rest of the path, so it must be the whole last segment, \
                     e.g. `/files/{{{capture}}}`; move it to the end"
                ),
            ));
        }
        segments.push(Segment::Capture { name: name.to_owned(), catch_all });
    }
    if !rest.is_empty() {
        segments.push(Segment::Literal(rest.to_owned()));
    }
    Ok(segments)
}

/// `Param<T>` → `Some(T)`.
fn param_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    if last.ident != "Param" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else { return None };
    match args.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let Args { method, path, pipeline } = syn::parse2(attr)?;
    let func: ItemFn = syn::parse2(item)?;
    let fn_name = &func.sig.ident;
    if func.sig.asyncness.is_none() {
        return Err(Error::new_spanned(func.sig.fn_token, "actions must be `async fn`"));
    }
    let segments = parse_path(&path)?;
    let captures: Vec<&str> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Capture { name, .. } => Some(name.as_str()),
            Segment::Literal(_) => None,
        })
        .collect();

    // For introspection: each argument and the return type as the author wrote them.
    let type_text = |ty: &dyn quote::ToTokens| crate::util::type_text(&quote!(#ty).to_string());
    let arg_specs: Vec<TokenStream> = func
        .sig
        .inputs
        .iter()
        .filter_map(|input| match input {
            FnArg::Typed(arg) => {
                let name = match &*arg.pat {
                    Pat::Ident(pat) => pat.ident.to_string(),
                    _ => "_".to_owned(),
                };
                let ty = type_text(&arg.ty);
                Some(quote! { (#name, #ty) })
            }
            FnArg::Receiver(_) => None,
        })
        .collect();
    let returns = match &func.sig.output {
        syn::ReturnType::Default => "()".to_owned(),
        syn::ReturnType::Type(_, ty) => type_text(ty),
    };
    let mut wrapper_params = Vec::new();
    let mut call_args = Vec::new();
    let mut param_lets = Vec::new();
    let mut param_types: Vec<(String, &Type)> = Vec::new();

    for (i, input) in func.sig.inputs.iter().enumerate() {
        let FnArg::Typed(arg) = input else {
            return Err(Error::new_spanned(input, "actions are free functions; `self` is not allowed"));
        };
        let Pat::Ident(pat) = &*arg.pat else {
            return Err(Error::new_spanned(&arg.pat, "action arguments must be plain `name: Type` bindings"));
        };
        let ident = &pat.ident;
        if let Some(inner) = param_inner(&arg.ty) {
            let name = ident.to_string();
            if !captures.contains(&name.as_str()) {
                return Err(Error::new_spanned(
                    ident,
                    format!("`{name}` is a `Param`, but the route `{}` has no `{{{name}}}` segment", path.value()),
                ));
            }
            param_lets.push(quote! {
                let #ident = match __kiln_params.parse::<#inner>(#name) {
                    ::core::result::Result::Ok(__kiln_v) => ::kiln::Param(__kiln_v),
                    ::core::result::Result::Err(__kiln_e) => return ::kiln::__private::IntoResponse::into_response(__kiln_e),
                };
            });
            param_types.push((name, inner));
            call_args.push(quote! { #ident });
        } else {
            let ty = &arg.ty;
            let w = format_ident!("__kiln_arg{}", i);
            wrapper_params.push(quote! { #w: #ty });
            call_args.push(quote! { #w });
        }
    }

    for capture in &captures {
        if !param_types.iter().any(|(n, _)| n == capture) {
            return Err(Error::new(
                path.span(),
                format!("the route captures `{{{capture}}}`, but `{fn_name}` has no `{capture}: Param<_>` argument"),
            ));
        }
    }

    let type_of =
        |capture: &str| param_types.iter().find(|(n, _)| n == capture).map(|(_, t)| *t).expect("checked above");
    let path_args = captures.iter().map(|c| {
        let (ident, ty) = (format_ident!("{}", c), type_of(c));
        quote! { #ident: impl ::kiln::PathArg<#ty> }
    });
    let pushes = segments.iter().map(|s| match s {
        Segment::Literal(lit) => quote! { __kiln_path.push_str(#lit); },
        Segment::Capture { name, catch_all } => {
            let (ident, ty) = (format_ident!("{}", name), type_of(name));
            let encode = if *catch_all { quote!(segments) } else { quote!(segment) };
            quote! { __kiln_path.push_str(&::kiln::PathArg::<#ty>::#encode(&#ident)); }
        }
    });

    let raw_params = (!captures.is_empty()).then(|| quote! { __kiln_params: ::kiln::__private::RawParams, });
    let method_str = method.to_string();
    let fn_name_str = fn_name.to_string();
    let path_doc = format!("Path to `{method_str} {}`, built from typed arguments.", path.value());

    Ok(quote! {
        #func

        #[doc = #path_doc]
        pub fn path(#( #path_args ),*) -> ::std::string::String {
            let mut __kiln_path = ::std::string::String::new();
            #( #pushes )*
            __kiln_path
        }

        #[doc(hidden)]
        async fn __kiln_handler(#raw_params #( #wrapper_params ),*) -> ::kiln::__private::Response {
            #( #param_lets )*
            ::kiln::__private::IntoResponse::into_response(#fn_name(#( #call_args ),*).await)
        }

        #[doc(hidden)]
        fn __kiln_mount(__kiln_router: ::kiln::__private::Router) -> ::kiln::__private::Router {
            __kiln_router.route(
                #path,
                ::kiln::__private::axum::routing::on(::kiln::__private::axum::routing::MethodFilter::#method, __kiln_handler),
            )
        }

        ::kiln::__private::inventory::submit! {
            ::kiln::RouteDef {
                method: #method_str,
                path: #path,
                module: ::core::module_path!(),
                function: #fn_name_str,
                file: ::core::file!(),
                line: ::core::line!(),
                pipeline: #pipeline,
                args: &[#( #arg_specs ),*],
                returns: #returns,
                mount: __kiln_mount,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(path: &str) -> Result<Vec<Segment>, String> {
        parse_path(&LitStr::new(path, proc_macro2::Span::call_site())).map_err(|e| e.to_string())
    }

    #[test]
    fn catch_all_is_the_whole_last_segment() {
        assert_eq!(
            parse("/{owner}/tree/{*path}").unwrap(),
            [
                Segment::Literal("/".into()),
                Segment::Capture { name: "owner".into(), catch_all: false },
                Segment::Literal("/tree/".into()),
                Segment::Capture { name: "path".into(), catch_all: true },
            ]
        );
        for misplaced in ["/{*path}/edit", "/files-{*path}", "/{a}{*path}"] {
            assert!(parse(misplaced).unwrap_err().contains("must be the whole last segment"), "{misplaced}");
        }
        assert!(parse("/{*Path}").unwrap_err().contains("use `{*snake_case_name}`"));
        assert!(parse("/{*}").unwrap_err().contains("use `{*snake_case_name}`"));
        assert!(parse("/{Slug}").unwrap_err().contains("use `{snake_case_name}`"));
    }
}
