//! `blog db reset` against a `DATABASE_URL` of its own, as a second checkout would use, and
//! its refusals.

use std::{path::PathBuf, process::Command};

use blog::records::*;
use kiln::prelude::*;

fn blog(database_url: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(args)
        .env("DATABASE_URL", database_url)
        .env("KILN_ENV", "development")
        .env_remove("QUEUE_DATABASE_URL")
        .env_remove("CACHE_DATABASE_URL")
        .env_remove("STREAMS_DATABASE_URL")
        .output()
        .expect("the blog binary runs");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("db-reset-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn resets_the_sqlite_file_database_url_names() -> Result {
    let dir = scratch("sqlite");
    let url = format!("sqlite://{}", dir.join("feature.sqlite3").display());

    let (ok, out) = blog(&url, &["db", "reset"]);
    assert!(ok, "{out}");
    assert!(out.contains(&format!("Resetting {url}")), "{out}");
    let seeded = {
        let db = Db::connect(&url).await?;
        User::query().count(&db).await?
    };
    assert!(seeded > 0);

    // Data added since is gone after another reset: the file was recreated, not reused.
    {
        let db = Db::connect(&url).await?;
        User::insert(NewUser {
            name: "Extra".into(),
            email: "extra@example.com".into(),
            password_digest: String::new(),
            confirmed_at: None,
        })
        .run(&db)
        .await?;
    }
    let (ok, out) = blog(&url, &["db", "reset"]);
    assert!(ok, "{out}");
    let db = Db::connect(&url).await?;
    assert_eq!(User::query().count(&db).await?, seeded);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn refuses_a_remote_server_without_allow_remote() {
    // Refused before connecting, so the host needn't exist.
    let (ok, out) = blog("postgres://admin:hunter2@db.example.invalid/blog", &["db", "reset"]);
    assert!(!ok);
    assert!(out.contains("postgres://admin:***@db.example.invalid/blog") && out.contains("--allow-remote"), "{out}");
    assert!(!out.contains("hunter2"), "{out}");
}

#[test]
fn refuses_in_production() {
    let dir = scratch("production");
    let url = format!("sqlite://{}", dir.join("production.sqlite3").display());
    let out = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["db", "reset", "--allow-remote"])
        .env("DATABASE_URL", &url)
        .env("KILN_ENV", "production")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("doesn't run in production"));
    assert!(!dir.join("production.sqlite3").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
