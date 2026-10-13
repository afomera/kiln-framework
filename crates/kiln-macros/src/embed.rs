use proc_macro2::TokenStream;
use quote::quote;

/// What a deployed app reads at runtime, relative to its crate.
const DIRS: &[&str] = &["templates", "assets", "public", "db/migrations"];
const FILES: &[&str] = &["db/snapshot.json", "config/secrets.enc"];

/// `kiln::embedded_files!()`: the app's templates, assets, public files, migrations, schema
/// snapshot and encrypted secrets, compiled into release builds (debug builds read from
/// disk, so they get nothing).
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    if !input.is_empty() {
        return Err(syn::Error::new_spanned(input, "`embedded_files!()` takes no arguments"));
    }
    let root = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .map_err(|_| syn::Error::new(proc_macro2::Span::call_site(), "no CARGO_MANIFEST_DIR"))?,
    );
    let mut found: Vec<String> = FILES.iter().filter(|f| root.join(f).is_file()).map(|f| (*f).to_owned()).collect();
    for dir in DIRS {
        walk(&root, &root.join(dir), &mut found);
    }
    found.sort();
    let entries = found.iter().map(|relative| {
        let absolute = root.join(relative).to_string_lossy().into_owned();
        quote! { (#relative, ::core::include_bytes!(#absolute)) }
    });
    Ok(quote! {
        {
            #[cfg(not(debug_assertions))]
            const FILES: &[(&str, &[u8])] = &[#( #entries ),*];
            #[cfg(debug_assertions)]
            const FILES: &[(&str, &[u8])] = &[];
            ::kiln::Embedded { package: ::core::env!("CARGO_PKG_NAME"), files: FILES }
        }
    })
}

fn walk(root: &std::path::Path, dir: &std::path::Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, found);
        } else if let Ok(relative) = path.strip_prefix(root) {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}
