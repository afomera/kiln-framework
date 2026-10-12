use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::{
    Data, DeriveInput, Error, Fields, Ident, LitStr, Path, Token, Type,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

use crate::util::snake_case;

const AUTO_FIELDS: [&str; 3] = ["id", "created_at", "updated_at"];

#[derive(Clone, Copy, PartialEq)]
enum RelKind {
    BelongsTo,
    HasMany,
}

struct RelSpec {
    name: Ident,
    target: Path,
}

impl Parse for RelSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![:]>()?;
        let target = input.parse()?;
        Ok(Self { name, target })
    }
}

struct Rel {
    kind: RelKind,
    name: Ident,
    target: Path,
    /// `belongs_to` whose foreign key is `Option<Id<T>>`.
    optional: bool,
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(tp) if tp.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

#[derive(Clone, Copy, PartialEq)]
enum Index {
    None,
    Plain,
    Unique,
}

struct Field<'a> {
    ident: &'a Ident,
    ty: &'a Type,
    index: Index,
}

impl Field<'_> {
    fn is_auto(&self) -> bool {
        AUTO_FIELDS.contains(&self.ident.to_string().as_str())
    }
}

pub fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let vis = &input.vis;
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(&input.generics, "a Record cannot be generic"));
    }
    let named = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => &f.named,
            _ => return Err(Error::new_spanned(name, "a Record must have named fields")),
        },
        _ => return Err(Error::new_spanned(name, "Record can only be derived for structs")),
    };

    let mut table = None;
    let mut parts = false;
    let mut rels = Vec::new();
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("record")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let lit: LitStr = meta.value()?.parse()?;
                table = Some(lit.value());
                return Ok(());
            }
            if meta.path.is_ident("parts") {
                parts = true;
                return Ok(());
            }
            let kind = if meta.path.is_ident("belongs_to") {
                RelKind::BelongsTo
            } else if meta.path.is_ident("has_many") {
                RelKind::HasMany
            } else {
                return Err(meta.error(
                    "unknown `record` option; expected `table = \"..\"`, `belongs_to(..)`, `has_many(..)` or `parts`",
                ));
            };
            let content;
            syn::parenthesized!(content in meta.input);
            for spec in Punctuated::<RelSpec, Token![,]>::parse_terminated(&content)? {
                rels.push(Rel { kind, name: spec.name, target: spec.target, optional: false });
            }
            Ok(())
        })?;
    }
    let table = table.unwrap_or_else(|| format!("{}s", snake_case(&name.to_string())));

    let mut fields = Vec::new();
    for f in named {
        let ident = f.ident.as_ref().expect("named field");
        let mut index = Index::None;
        for attr in f.attrs.iter().filter(|a| a.path().is_ident("index")) {
            index = Index::Plain;
            if matches!(attr.meta, syn::Meta::List(_)) {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("unique") {
                        index = Index::Unique;
                        Ok(())
                    } else {
                        Err(meta.error("expected `unique`"))
                    }
                })?;
            }
        }
        fields.push(Field { ident, ty: &f.ty, index });
    }

    if !fields.iter().any(|f| f.ident == "id") {
        return Err(Error::new_spanned(name, format!("a Record needs a primary key field `id: Id<{name}>`")));
    }
    for rel in rels.iter_mut().filter(|r| r.kind == RelKind::BelongsTo) {
        let fk = format!("{}_id", rel.name);
        let Some(field) = fields.iter().find(|f| f.ident == fk.as_str()) else {
            return Err(Error::new_spanned(
                &rel.name,
                format!("`belongs_to({}: ..)` needs a foreign key field `{fk}` on `{name}`", rel.name),
            ));
        };
        rel.optional = is_option(field.ty);
    }
    for rel in &rels {
        if fields.iter().any(|f| *f.ident == rel.name) {
            return Err(Error::new_spanned(
                &rel.name,
                format!("relation `{}` has the same name as a column", rel.name),
            ));
        }
    }

    let has_created_at = fields.iter().any(|f| f.ident == "created_at");
    let has_updated_at = fields.iter().any(|f| f.ident == "updated_at");
    let new_name = format_ident!("New{}", name);
    let trait_name = format_ident!("{}Relations", name);

    let column_consts = fields.iter().map(|f| {
        let (ident, ty) = (f.ident, f.ty);
        let doc = format!("Column `{table}.{ident}`.");
        quote! {
            #[doc = #doc]
            #[allow(non_upper_case_globals)]
            #vis const #ident: ::kiln::Column<#name, #ty> = ::kiln::Column::new(stringify!(#ident));
        }
    });

    let column_names = fields.iter().map(|f| f.ident.to_string());

    let from_row_fields = fields.iter().map(|f| {
        let (ident, ty) = (f.ident, f.ty);
        quote! { #ident: <#ty as ::kiln::FromColumn>::from_column(__kiln_row, stringify!(#ident))? }
    });

    let new_fields = fields.iter().filter(|f| !f.is_auto()).map(|f| {
        let (ident, ty) = (f.ident, f.ty);
        quote! { pub #ident: #ty }
    });

    let new_values = fields.iter().filter(|f| !f.is_auto()).map(|f| {
        let ident = f.ident;
        quote! { (stringify!(#ident), ::kiln::ToValue::to_value(&__kiln_new.#ident)) }
    });
    let created_at_value =
        has_created_at.then(|| quote! { __kiln_values.push(("created_at", ::kiln::ToValue::to_value(&__kiln_now))); });
    let updated_at_value =
        has_updated_at.then(|| quote! { __kiln_values.push(("updated_at", ::kiln::ToValue::to_value(&__kiln_now))); });

    let column_schemas = fields.iter().map(|f| {
        let (ident, ty) = (f.ident, f.ty);
        let primary_key = f.ident == "id";
        let unique = f.index == Index::Unique;
        let indexed = f.index != Index::None;
        let references = rels
            .iter()
            .find(|r| r.kind == RelKind::BelongsTo && f.ident == format!("{}_id", r.name).as_str())
            .map(|r| {
                let target = &r.target;
                quote! { ::core::option::Option::Some(<#target as ::kiln::Record>::TABLE) }
            })
            .unwrap_or_else(|| quote! { ::core::option::Option::None });
        quote! {
            ::kiln::ColumnSchema {
                name: stringify!(#ident),
                sql_type: <#ty as ::kiln::SqlType>::SQL,
                nullable: <#ty as ::kiln::SqlType>::NULLABLE,
                primary_key: #primary_key,
                unique: #unique,
                indexed: #indexed,
                references: #references,
                variants: <#ty as ::kiln::SqlType>::VARIANTS,
            }
        }
    });

    let serialize_fields = fields.iter().map(|f| {
        let ident = f.ident;
        quote! { ::kiln::__private::serde::ser::SerializeStruct::serialize_field(&mut __kiln_state, stringify!(#ident), &self.#ident)?; }
    });
    let field_count = fields.len();
    let (part_count, serialize_parts, parts_enabled) = if parts {
        (
            quote! { + <Self as ::kiln::Part>::NAMES.len() },
            quote! { <Self as ::kiln::Part>::serialize_parts(self, &mut __kiln_state)?; },
            quote! { impl ::kiln::PartsEnabled for #name {} },
        )
    } else {
        (quote! {}, quote! {}, quote! {})
    };

    let relations = rels.iter().map(|rel| relation_items(name, vis, rel));

    let accessor_sigs: Vec<TokenStream> = rels
        .iter()
        .map(|rel| {
            let (rname, target) = (&rel.name, &rel.target);
            let doc = format!("The preloaded `{rname}` relation. Only callable when `{rname}` was preloaded.");
            quote! {
                #[doc = #doc]
                fn #rname<__KilnI, __KilnN>(&self) -> &<#rname<__KilnN> as ::kiln::Relation<#name>>::Out
                where
                    Self: ::kiln::HasRelation<#name, #rname<__KilnN>, __KilnI>,
                    __KilnN: ::kiln::RelSet<#target>
            }
        })
        .collect();
    let accessor_impls = rels.iter().zip(&accessor_sigs).map(|(rel, sig)| {
        let rname = &rel.name;
        quote! {
            #sig {
                <Self as ::kiln::HasRelation<#name, #rname<__KilnN>, __KilnI>>::relation(self)
            }
        }
    });
    let accessors = (!rels.is_empty()).then(|| {
        let doc = format!("Named accessors for relations preloaded on `{name}`.");
        quote! {
            #[doc = #doc]
            #vis trait #trait_name: ::core::marker::Sized {
                #( #accessor_sigs; )*
            }

            impl<__KilnR: ::kiln::RelSet<#name>> #trait_name for ::kiln::Loaded<#name, __KilnR> {
                #( #accessor_impls )*
            }
        }
    });

    let new_doc = format!("Insertable fields for `{name}`; `id` and timestamps are set by the database and kiln.");
    let name_str = name.to_string();

    Ok(quote! {
        #( #column_consts )*

        #( #relations )*

        #accessors

        #[doc = #new_doc]
        #[derive(Debug, Clone)]
        #vis struct #new_name {
            #( #new_fields, )*
        }

        impl ::kiln::Record for #name {
            const TABLE: &'static str = #table;
            const COLUMNS: &'static [&'static str] = &[#( #column_names ),*];
            const HAS_UPDATED_AT: bool = #has_updated_at;

            type New = #new_name;

            fn id(&self) -> ::kiln::Id<Self> {
                self.id
            }

            fn from_row(__kiln_row: &::kiln::__private::SqliteRow) -> ::core::result::Result<Self, ::kiln::__private::sqlx::Error> {
                ::core::result::Result::Ok(Self { #( #from_row_fields, )* })
            }

            fn new_values(__kiln_new: Self::New) -> ::std::vec::Vec<(&'static str, ::kiln::Value)> {
                #[allow(unused_variables)]
                let __kiln_now = ::kiln::Timestamp::now();
                #[allow(unused_mut)]
                let mut __kiln_values = ::std::vec![#( #new_values ),*];
                #created_at_value
                #updated_at_value
                __kiln_values
            }

            fn schema() -> ::kiln::TableSchema {
                ::kiln::TableSchema { table: #table, columns: ::std::vec![#( #column_schemas ),*] }
            }
        }

        impl ::kiln::__private::serde::Serialize for #name {
            fn serialize<__KilnS: ::kiln::__private::serde::Serializer>(&self, __kiln_serializer: __KilnS) -> ::core::result::Result<__KilnS::Ok, __KilnS::Error> {
                let mut __kiln_state = __kiln_serializer.serialize_struct(#name_str, #field_count #part_count)?;
                #( #serialize_fields )*
                #serialize_parts
                ::kiln::__private::serde::ser::SerializeStruct::end(__kiln_state)
            }
        }

        #parts_enabled

        ::kiln::__private::inventory::submit! {
            ::kiln::RecordDef { name: #name_str, table: #table, schema: <#name as ::kiln::Record>::schema }
        }
    })
}

fn relation_items(owner: &Ident, vis: &syn::Visibility, rel: &Rel) -> TokenStream {
    let (rname, target) = (&rel.name, &rel.target);
    let span = rname.span();
    let (out, loader) = match rel.kind {
        RelKind::BelongsTo if rel.optional => {
            let fk = format_ident!("{}_id", rname);
            (
                quote! { ::core::option::Option<<__KilnN as ::kiln::RelSet<#target>>::Row> },
                quote_spanned! {span=>
                    ::kiln::rel::load_belongs_to_optional::<#owner, #target, __KilnN, _>(
                        __kiln_owners,
                        |__kiln_owner: &#owner| __kiln_owner.#fk,
                        __kiln_db,
                    )
                },
            )
        }
        RelKind::BelongsTo => {
            let fk = format_ident!("{}_id", rname);
            (
                quote! { <__KilnN as ::kiln::RelSet<#target>>::Row },
                quote_spanned! {span=>
                    ::kiln::rel::load_belongs_to::<#owner, #target, __KilnN, _>(
                        __kiln_owners,
                        |__kiln_owner: &#owner| __kiln_owner.#fk,
                        __kiln_db,
                    )
                },
            )
        }
        RelKind::HasMany => {
            // Convention: the child table has `{owner}_id`, e.g. `comments.post_id`.
            let fk = format_ident!("{}_id", snake_case(&owner.to_string()));
            (
                quote! { ::std::vec::Vec<<__KilnN as ::kiln::RelSet<#target>>::Row> },
                quote_spanned! {span=>
                    ::kiln::rel::load_has_many::<#owner, #target, __KilnN, _>(
                        __kiln_owners,
                        ::kiln::Column::<#target, ::kiln::Id<#owner>>::new(stringify!(#fk)),
                        |__kiln_child: &#target| __kiln_child.#fk,
                        __kiln_db,
                    )
                },
            )
        }
    };
    let kind = match rel.kind {
        RelKind::BelongsTo => "belongs_to",
        RelKind::HasMany => "has_many",
    };
    let doc = format!(
        "Relation `{owner}::{rname}` ({kind} `{}`). Pass to `.preload(..)`; nest with `{rname}.preload(..)`.",
        quote!(#target)
    );

    quote! {
        #[doc = #doc]
        #[allow(non_camel_case_types)]
        #vis struct #rname<__KilnN = ()> {
            _kiln: ::core::marker::PhantomData<fn() -> __KilnN>,
        }

        #[allow(non_upper_case_globals)]
        #vis const #rname: #rname = #rname { _kiln: ::core::marker::PhantomData };

        impl<__KilnN> ::core::default::Default for #rname<__KilnN> {
            fn default() -> Self {
                #rname { _kiln: ::core::marker::PhantomData }
            }
        }

        impl<__KilnN> ::core::clone::Clone for #rname<__KilnN> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<__KilnN> ::core::marker::Copy for #rname<__KilnN> {}

        impl<__KilnN> #rname<__KilnN> {
            /// Preloads a relation of the related record (nested preloading).
            pub fn preload<__KilnX>(self, _relation: __KilnX) -> #rname<<__KilnN as ::kiln::Append<__KilnX>>::Output>
            where
                __KilnN: ::kiln::Append<__KilnX>,
                __KilnX: ::kiln::Relation<#target>,
            {
                #rname { _kiln: ::core::marker::PhantomData }
            }
        }

        impl<__KilnN: ::kiln::RelSet<#target>> ::kiln::Relation<#owner> for #rname<__KilnN> {
            type Out = #out;
            const NAME: &'static str = stringify!(#rname);

            fn load<'a>(
                __kiln_owners: &'a [#owner],
                __kiln_db: &'a ::kiln::Db,
            ) -> ::kiln::BoxFuture<'a, ::kiln::Result<::std::vec::Vec<Self::Out>>> {
                ::std::boxed::Box::pin(#loader)
            }
        }
    }
}
