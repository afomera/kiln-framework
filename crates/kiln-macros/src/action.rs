use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Error, FnArg, GenericArgument, Ident, ItemFn, LitStr, Pat, PathArguments, Type,
    parse::{Parse, ParseStream},
};

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

struct Args {
    method: Ident,
    path: LitStr,
    pipeline: String,
    /// `scope = "repo:write"`, the API token scope the route needs (`"any"`: any token).
    scope: Option<LitStr>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let method: Ident = input.parse()?;
        if !METHODS.contains(&method.to_string().as_str()) {
            return Err(Error::new_spanned(&method, format!("expected an HTTP method: {}", METHODS.join(", "))));
        }
        let path: LitStr = input.parse()?;
        let mut pipeline = "browser".to_owned();
        let mut scope = None;
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            if key == "scope" {
                let value: LitStr = input.parse()?;
                let text = value.value();
                let valid = !text.is_empty()
                    && text.len() <= 64
                    && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'));
                if !valid {
                    return Err(Error::new_spanned(
                        &value,
                        "a scope is up to 64 letters, digits and `:._-`, like `repo:write` (or `any`)",
                    ));
                }
                scope = Some(value);
                continue;
            }
            let value: Ident = input.parse()?;
            match (key.to_string().as_str(), value.to_string().as_str()) {
                ("pipeline", "Browser") => pipeline = "browser".into(),
                ("pipeline", "Api") => pipeline = "api".into(),
                ("pipeline", _) => return Err(Error::new_spanned(value, "expected `Browser` (the default) or `Api`")),
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "unknown option; expected `pipeline = Browser | Api` or `scope = \"..\"`",
                    ));
                }
            }
        }
        if let Some(scope) = &scope
            && pipeline != "api"
        {
            return Err(Error::new_spanned(
                scope,
                "`scope` is for API tokens, which only `pipeline = Api` routes accept; add `pipeline = Api` or remove it",
            ));
        }
        Ok(Self { method, path, pipeline, scope })
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

/// The type's last path segment, looking through `Option<..>`: `Option<Form<PostForm>>` → `Form`.
fn last_ident(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    if last.ident == "Option"
        && let PathArguments::AngleBracketed(args) = &last.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return last_ident(inner);
    }
    Some(last.ident.to_string())
}

/// Extractors that read the request body, which only the last argument may do.
const BODY_EXTRACTORS: [&str; 8] = ["Form", "Json", "Body", "Bytes", "String", "Request", "Multipart", "RawForm"];

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let Args { method, path, pipeline, scope } = syn::parse2(attr)?;
    let func: ItemFn = syn::parse2(item)?;
    let fn_name = &func.sig.ident;
    if func.sig.asyncness.is_none() {
        return Err(Error::new_spanned(func.sig.fn_token, "actions must be `async fn`"));
    }
    if fn_name == "path" {
        return Err(Error::new_spanned(
            fn_name,
            "an action can't be named `path`: `#[action]` generates `path(..)` beside it, the URL helper; rename the function (`show`, `index`, ..)",
        ));
    }
    let segments = parse_path(&path)?;
    let captures: Vec<&str> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Capture { name, .. } => Some(name.as_str()),
            Segment::Literal(_) => None,
        })
        .collect();
    for (i, capture) in captures.iter().enumerate() {
        if captures[..i].contains(capture) {
            return Err(Error::new(
                path.span(),
                format!("the route captures `{{{capture}}}` twice; give each capture its own name"),
            ));
        }
    }

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
    // Deny by default: an API route that signs someone in says which token scope it needs,
    // so forgetting a check can't let a narrow token (a read-only CI one) do anything.
    let signs_in = func.sig.inputs.iter().any(|input| match input {
        FnArg::Typed(arg) => type_text(&arg.ty).contains("Current<"),
        FnArg::Receiver(_) => false,
    });
    if pipeline == "api" && signs_in && scope.is_none() {
        return Err(Error::new_spanned(
            &path,
            "API routes that take `Current<..>` say which token scope they need: add `scope = \"repo:read\"` (the scope a token must have) or `scope = \"any\"` (any token)",
        ));
    }
    let scope_value = match &scope {
        Some(scope) => quote! { ::core::option::Option::Some(#scope) },
        None => quote! { ::core::option::Option::None },
    };
    let returns = match &func.sig.output {
        syn::ReturnType::Default => "()".to_owned(),
        syn::ReturnType::Type(_, ty) => type_text(ty),
    };
    let mut wrapper_params = Vec::new();
    let mut call_args = Vec::new();
    let mut param_lets = Vec::new();
    let mut param_types: Vec<(String, &Type)> = Vec::new();
    let mut wrapper_types: Vec<&Type> = Vec::new();

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
            // Bound under its own name, so a capture named like the action (`tab` in
            // `fn tab(.., tab: Param<String>)`) doesn't shadow the function being called.
            let local = format_ident!("__kiln_param_{}", ident);
            param_lets.push(quote! {
                let #local = match __kiln_params.parse::<#inner>(#name) {
                    ::core::result::Result::Ok(__kiln_v) => ::kiln::Param(__kiln_v),
                    ::core::result::Result::Err(__kiln_e) => return ::kiln::__private::IntoResponse::into_response(__kiln_e),
                };
            });
            param_types.push((name, inner));
            call_args.push(quote! { #local });
        } else {
            let ty = &arg.ty;
            let w = format_ident!("__kiln_arg{}", i);
            wrapper_params.push(quote! { #w: #ty });
            wrapper_types.push(ty);
            call_args.push(quote! { #w });
        }
    }
    // Each argument is checked on its own, spanned to its type, so a type that isn't an
    // extractor is named (see `kiln::routing::ActionArg`).
    let arg_checks = wrapper_types.iter().enumerate().map(|(i, ty)| {
        if i + 1 == wrapper_types.len() {
            quote_spanned! {ty.span()=> ::kiln::__private::last_action_arg::<#ty, _>(); }
        } else {
            quote_spanned! {ty.span()=> ::kiln::__private::action_arg::<#ty>(); }
        }
    });

    for capture in &captures {
        if !param_types.iter().any(|(n, _)| n == capture) {
            // The usual slip: the argument is there, but not wrapped in `Param`.
            let same_name = func.sig.inputs.iter().find_map(|input| match input {
                FnArg::Typed(arg) if matches!(&*arg.pat, Pat::Ident(p) if p.ident == capture) => Some(arg),
                _ => None,
            });
            if let Some(arg) = same_name {
                let ty = &arg.ty;
                return Err(Error::new_spanned(
                    ty,
                    format!(
                        "`{capture}` is the route's `{{{capture}}}` capture, so it's read with `Param`: write `{capture}: Param<{}>`",
                        type_text(ty)
                    ),
                ));
            }
            return Err(Error::new(
                path.span(),
                format!("the route captures `{{{capture}}}`, but `{fn_name}` has no `{capture}: Param<_>` argument"),
            ));
        }
    }

    // Body extractors must come last: axum hands the body to the last argument only, and
    // otherwise fails with an opaque `Handler` error. `Param`s don't count: they aren't
    // arguments of the generated handler.
    let extractors: Vec<&syn::PatType> = func
        .sig
        .inputs
        .iter()
        .filter_map(|input| match input {
            FnArg::Typed(arg) if param_inner(&arg.ty).is_none() => Some(arg),
            _ => None,
        })
        .collect();
    let reads_body = |arg: &&syn::PatType| last_ident(&arg.ty).is_some_and(|i| BODY_EXTRACTORS.contains(&i.as_str()));
    if let Some(at) = extractors.iter().position(reads_body)
        && at + 1 < extractors.len()
    {
        let body = extractors[at];
        let kind = last_ident(&body.ty).unwrap_or_default();
        let shown = if matches!(kind.as_str(), "String" | "Bytes" | "Body" | "Request") {
            format!("`{kind}`")
        } else {
            format!("`{kind}<..>`")
        };
        let after: Vec<String> = extractors[at + 1..].iter().map(|arg| arg.pat.to_token_stream().to_string()).collect();
        return Err(Error::new_spanned(
            &body.ty,
            format!(
                "{shown} reads the request body, so it must be the last parameter; move `{}` after `{}`",
                body.pat.to_token_stream(),
                after.join("`, `"),
            ),
        ));
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
            #( #arg_checks )*
            #( #param_lets )*
            ::kiln::__private::IntoResponse::into_response(#fn_name(#( #call_args ),*).await)
        }

        #[doc(hidden)]
        fn __kiln_mount(__kiln_router: ::kiln::__private::Router) -> ::kiln::__private::Router {
            __kiln_router.route(
                #path,
                ::kiln::__private::axum::routing::on(::kiln::__private::axum::routing::MethodFilter::#method, __kiln_handler)
                    .layer(::kiln::__private::axum::Extension(::kiln::__private::RequiredScope(#scope_value))),
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
                scope: #scope_value,
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
