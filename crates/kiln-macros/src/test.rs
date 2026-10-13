use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Error, ItemFn};

/// `#[kiln::test] async fn name(app: TestApp) -> kiln::Result { .. }` becomes a tokio test
/// that boots a fresh in-memory app with the crate's migrations applied.
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(Error::new_spanned(attr, "`#[kiln::test]` takes no arguments"));
    }
    let mut func: ItemFn = syn::parse2(item)?;
    if func.sig.asyncness.is_none() {
        return Err(Error::new_spanned(func.sig.fn_token, "kiln tests are `async fn`"));
    }
    if func.sig.inputs.len() != 1 {
        return Err(Error::new_spanned(&func.sig.inputs, "a kiln test takes one argument: `app: TestApp`"));
    }
    let name = func.sig.ident.clone();
    let output = func.sig.output.clone();
    let attrs = std::mem::take(&mut func.attrs);
    let inner = format_ident!("__kiln_test_{}", name);
    func.sig.ident = inner.clone();

    // Apps keep their settings in `src/app.rs` (`pub fn configure(App) -> App`); tests use
    // them too, so behavior like the sign-in redirect matches the running app.
    let configure = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .filter(|dir| std::path::Path::new(dir).join("src/app.rs").exists())
        .and_then(|_| std::env::var("CARGO_PKG_NAME").ok())
        .map(|package| {
            let krate = format_ident!("{}", package.replace('-', "_"));
            quote! { ::#krate::app::configure }
        })
        .unwrap_or_else(|| quote! { ::core::result::Result::Ok });

    Ok(quote! {
        #( #attrs )*
        #[::kiln::__private::tokio::test(crate = "::kiln::__private::tokio")]
        async fn #name() #output {
            let __kiln_app = ::kiln::testing::TestApp::configured(
                ::std::path::Path::new(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/db/migrations")),
                #configure,
            )
            .await
            .expect("the test app boots and its migrations apply");
            #inner(__kiln_app).await
        }

        #func
    })
}
