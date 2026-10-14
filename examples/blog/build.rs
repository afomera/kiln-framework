//! Release builds carry the app's templates, assets, migrations and secrets inside the
//! binary (`kiln::embedded_files!()` in src/main.rs), so a change to any of them rebuilds it.
//! Debug builds read them from disk and don't need to.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    for path in [
        "templates",
        "assets",
        "public",
        "db/migrations",
        "db/snapshot.json",
        "config/secrets.enc",
        "config/importmap.toml",
    ] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
