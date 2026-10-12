use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, FnArg, ImplItem, ItemImpl, ReturnType};

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(Error::new_spanned(attr, "`#[part]` takes no arguments"));
    }
    let block: ItemImpl = syn::parse2(item)?;
    if let Some((_, path, _)) = &block.trait_ {
        return Err(Error::new_spanned(path, "`#[part]` goes on an inherent impl block: `#[part] impl Post { .. }`"));
    }
    if !block.generics.params.is_empty() {
        return Err(Error::new_spanned(&block.generics, "a part cannot be generic"));
    }
    let self_ty = &block.self_ty;

    match part_names(&block) {
        Ok(names) => Ok(expand_part(&block, &names)),
        // Keep the impl and a stub `Part` so the only error shown is the real one.
        Err(e) => {
            let e = e.into_compile_error();
            Ok(quote! {
                #e
                #block
                impl ::kiln::Part for #self_ty {
                    const NAMES: &'static [&'static str] = &[];
                    fn serialize_parts<__KilnS: ::kiln::__private::serde::ser::SerializeStruct>(
                        &self,
                        _: &mut __KilnS,
                    ) -> ::core::result::Result<(), __KilnS::Error> {
                        ::core::result::Result::Ok(())
                    }
                }
            })
        }
    }
}

fn part_names(block: &ItemImpl) -> syn::Result<Vec<syn::Ident>> {
    let mut names = Vec::new();
    for item in &block.items {
        let ImplItem::Fn(method) = item else {
            return Err(Error::new_spanned(item, "a part contains only methods"));
        };
        let sig = &method.sig;
        let takes_ref_self =
            matches!(sig.inputs.first(), Some(FnArg::Receiver(r)) if r.reference.is_some() && r.mutability.is_none());
        if !takes_ref_self || sig.inputs.len() != 1 {
            return Err(Error::new_spanned(
                sig,
                "part methods take only `&self`, so templates can read them as values: `fn reading_time(&self) -> String`",
            ));
        }
        if sig.asyncness.is_some() || !sig.generics.params.is_empty() {
            return Err(Error::new_spanned(sig, "part methods are plain, non-generic, synchronous functions"));
        }
        if matches!(sig.output, ReturnType::Default) {
            return Err(Error::new_spanned(sig, "part methods return the value templates will see"));
        }
        names.push(sig.ident.clone());
    }
    Ok(names)
}

fn expand_part(block: &ItemImpl, names: &[syn::Ident]) -> TokenStream {
    let self_ty = &block.self_ty;
    let name_strs: Vec<String> = names.iter().map(ToString::to_string).collect();

    quote! {
        #block

        impl ::kiln::Part for #self_ty {
            const NAMES: &'static [&'static str] = &[#( #name_strs ),*];

            fn serialize_parts<__KilnS: ::kiln::__private::serde::ser::SerializeStruct>(
                &self,
                __kiln_state: &mut __KilnS,
            ) -> ::core::result::Result<(), __KilnS::Error> {
                #( __kiln_state.serialize_field(#name_strs, &self.#names())?; )*
                ::core::result::Result::Ok(())
            }
        }

        const _: () = {
            // The record must opt in with `#[record(.., parts)]`.
            fn __kiln_requires_parts_flag<T: ::kiln::PartsEnabled>() {}
            let _ = __kiln_requires_parts_flag::<#self_ty>;
            // Part names share the template namespace with columns.
            ::kiln::__private::assert_no_overlap(
                <#self_ty as ::kiln::Record>::COLUMNS,
                <#self_ty as ::kiln::Part>::NAMES,
            );
        };
    }
}
