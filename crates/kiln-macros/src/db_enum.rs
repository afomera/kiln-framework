use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields};

use crate::util::snake_case;

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return Err(Error::new_spanned(name, "DbEnum can only be derived for enums"));
    };
    let mut variants = Vec::new();
    let mut values = Vec::new();
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(Error::new_spanned(v, "DbEnum variants cannot carry data"));
        }
        variants.push(&v.ident);
        values.push(snake_case(&v.ident.to_string()));
    }
    let name_str = name.to_string();
    let expected = values.join(", ");

    Ok(quote! {
        impl #name {
            /// The stored text value.
            pub const fn as_str(&self) -> &'static str {
                match self { #( Self::#variants => #values, )* }
            }
        }

        impl ::core::fmt::Display for #name {
            fn fmt(&self, __kiln_f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                __kiln_f.write_str(self.as_str())
            }
        }

        impl ::core::str::FromStr for #name {
            type Err = ::kiln::Error;

            fn from_str(__kiln_s: &str) -> ::core::result::Result<Self, Self::Err> {
                match __kiln_s {
                    #( #values => ::core::result::Result::Ok(Self::#variants), )*
                    __kiln_other => ::core::result::Result::Err(::kiln::Error::BadRequest(
                        ::std::format!("invalid {} `{}`; expected one of: {}", #name_str, __kiln_other, #expected),
                    )),
                }
            }
        }

        impl ::kiln::ToValue for #name {
            fn to_value(&self) -> ::kiln::Value {
                ::kiln::Value::Text(::std::string::ToString::to_string(self.as_str()))
            }
        }

        impl ::kiln::FromColumn for #name {
            fn from_column(__kiln_row: &::kiln::__private::SqliteRow, __kiln_name: &str) -> ::core::result::Result<Self, ::kiln::__private::sqlx::Error> {
                let __kiln_raw = <::std::string::String as ::kiln::FromColumn>::from_column(__kiln_row, __kiln_name)?;
                __kiln_raw.parse().map_err(|__kiln_e: ::kiln::Error| ::kiln::__private::sqlx::Error::ColumnDecode {
                    index: ::std::string::ToString::to_string(__kiln_name),
                    source: ::std::boxed::Box::new(__kiln_e),
                })
            }
        }

        impl ::kiln::SqlType for #name {
            const SQL: &'static str = "TEXT";
            const VARIANTS: &'static [&'static str] = &[#( #values ),*];
        }

        impl ::kiln::__private::serde::Serialize for #name {
            fn serialize<__KilnS: ::kiln::__private::serde::Serializer>(&self, __kiln_serializer: __KilnS) -> ::core::result::Result<__KilnS::Ok, __KilnS::Error> {
                __kiln_serializer.serialize_str(self.as_str())
            }
        }
    })
}
