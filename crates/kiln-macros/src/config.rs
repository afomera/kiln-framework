use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, GenericArgument, LitStr, PathArguments, Type};

/// `Option<T>` → `Some(T)`.
fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    if last.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else { return None };
    match args.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(name, "Config can only be derived for structs"));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new_spanned(name, "a config struct must have named fields"));
    };
    let mut loads = Vec::new();
    let mut builds = Vec::new();
    for field in &named.named {
        let ident = field.ident.as_ref().expect("named field");
        let mut env = ident.to_string().to_uppercase();
        let mut default: Option<LitStr> = None;
        let mut dev_default: Option<LitStr> = None;
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("config")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("env") {
                    env = meta.value()?.parse::<LitStr>()?.value();
                } else if meta.path.is_ident("default") {
                    default = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("dev_default") {
                    dev_default = Some(meta.value()?.parse()?);
                } else {
                    return Err(meta.error("expected `env = \"..\"`, `default = \"..\"` or `dev_default = \"..\"`"));
                }
                Ok(())
            })?;
        }
        let default = default.map_or_else(|| quote!(None), |d| quote!(Some(#d)));
        let dev_default = dev_default.map_or_else(|| quote!(None), |d| quote!(Some(#d)));
        let local = quote::format_ident!("__kiln_{}", ident);
        match option_inner(&field.ty) {
            Some(inner) => {
                loads.push(quote! { let #local = __kiln_loader.optional::<#inner>(#env, #default, #dev_default); });
                builds.push(quote! { #ident: #local });
            }
            None => {
                let ty = &field.ty;
                loads.push(quote! { let #local = __kiln_loader.required::<#ty>(#env, #default, #dev_default); });
                builds.push(quote! { #ident: #local.expect("checked by finish()") });
            }
        }
    }
    Ok(quote! {
        impl ::kiln::config::Config for #name {
            fn load() -> ::kiln::Result<Self> {
                let mut __kiln_loader = ::kiln::config::Loader::new();
                #( #loads )*
                __kiln_loader.finish()?;
                ::core::result::Result::Ok(Self { #( #builds, )* })
            }
        }
    })
}
