use blog::{actions::posts, records::*};
use std::path::Path;

use kiln::{prelude::*, testing::TestApp};

/// A test app with the development seed data.
async fn app() -> TestApp {
    let migrations = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/db/migrations"));
    let app = TestApp::configured(migrations, blog::app::configure).await.unwrap();
    blog::seed(app.db.clone()).await.unwrap();
    app
}

#[tokio::test]
async fn root_redirects_to_posts() {
    let client = app().await.client();
    client.get("/").await.assert_redirect(&posts::index::path());
}

#[tokio::test]
async fn index_lists_published_posts_with_authors() {
    let client = app().await.client();
    client
        .get(posts::index::path())
        .await
        .assert_status(200)
        .assert_contains("Hello, Kiln")
        .assert_contains("Grace Hopper · 1 min read · 2 comments")
        .assert_contains(r#"href="/posts/typed-paths""#)
        .assert_not_contains("Draft post");
}

#[tokio::test]
async fn show_renders_nested_relations() {
    let client = app().await.client();
    client
        .get(posts::show::path("hello-kiln"))
        .await
        .assert_status(200)
        .assert_contains("<h1>Hello, Kiln</h1>")
        .assert_contains("Ada Lovelace · 2 comments")
        .assert_contains("<strong>Grace Hopper</strong>: Nice.");
}

#[tokio::test]
async fn unknown_slug_is_404() {
    let client = app().await.client();
    client.get(posts::show::path("nope")).await.assert_status(404);
}

#[test]
fn path_helpers_encode_segments() {
    assert_eq!(posts::show::path("a b/c"), "/posts/a%20b%2Fc");
    assert_eq!(posts::show::path(String::from("x")), "/posts/x");
}

#[test]
fn routes_are_registered_from_action_files() {
    let names: Vec<_> = kiln::routes()
        .iter()
        .map(|r| (r.method, r.path, r.name()))
        .filter(|(_, _, name)| ["home", "posts", "sessions"].iter().any(|p| name.starts_with(p)))
        .collect();
    assert_eq!(
        names,
        [
            ("GET", "/", "home::index"),
            ("GET", "/posts", "posts::index"),
            ("POST", "/posts", "posts::create"),
            ("GET", "/posts/new", "posts::new"),
            ("DELETE", "/posts/{slug}", "posts::destroy"),
            ("GET", "/posts/{slug}", "posts::show"),
            ("PATCH", "/posts/{slug}", "posts::update"),
            ("GET", "/posts/{slug}/edit", "posts::edit"),
            ("DELETE", "/session", "sessions::destroy"),
            ("POST", "/session", "sessions::create"),
            ("GET", "/session/new", "sessions::new"),
        ]
    );
}

/// Preloading costs one query per relation, however many rows come back.
#[tokio::test]
async fn preloading_is_one_query_per_relation() {
    let app = app().await;
    let db = &app.db;

    let before = db.query_count();
    let posts =
        Post::query().preload(post::author).preload(post::comments.preload(comment::author)).all(db).await.unwrap();
    // posts + authors + comments + comment authors
    assert_eq!(db.query_count() - before, 4);
    assert_eq!(posts.len(), 4);

    let total_comments: usize = posts.iter().map(|p| p.comments().len()).sum();
    assert_eq!(total_comments, 8);
    assert!(posts.iter().flat_map(|p| p.comments()).all(|c| !c.author().name.is_empty()));
}

#[tokio::test]
async fn plain_queries_return_plain_records() {
    let app = app().await;
    let db = &app.db;
    let post: Post = Post::find_by(post::slug.eq("hello-kiln")).one_or_404(db).await.unwrap();
    let author: User = User::find(post.author_id).one_or_404(db).await.unwrap();
    assert_eq!(author.name, "Ada Lovelace");
    assert_eq!(Post::published().count(db).await.unwrap(), 3);
}

#[tokio::test]
async fn update_sets_columns_and_touches_updated_at() {
    let app = app().await;
    let db = &app.db;
    let draft = Post::find_by(post::slug.eq("draft-post")).one_or_404(db).await.unwrap();
    let published = Post::update(draft.id)
        .set(post::status, PostStatus::Published)
        .set(post::published_at, Timestamp::now())
        .run(db)
        .await
        .unwrap();
    assert_eq!(published.status, PostStatus::Published);
    assert!(published.updated_at > draft.updated_at);
    assert_eq!(Post::published().count(db).await.unwrap(), 4);
}

#[test]
fn schema_comes_from_the_struct() {
    let sql = Post::schema().create_sql().join(";\n");
    assert!(sql.contains(r#""status" TEXT NOT NULL CHECK ("status" IN ('draft', 'published', 'archived'))"#), "{sql}");
    assert!(sql.contains(r#""author_id" INTEGER NOT NULL REFERENCES "users" ("id")"#), "{sql}");
    assert!(sql.contains(r#""published_at" INTEGER,"#), "{sql}");
    assert!(sql.contains(r#"CREATE UNIQUE INDEX "idx_posts_slug""#), "{sql}");
}

#[tokio::test]
async fn pages_render_inside_the_default_layout() {
    let client = app().await.client();
    client
        .get(posts::show::path("hello-kiln"))
        .await
        .assert_status(200)
        // `{% set title = post.title %}` in the page reaches the layout
        .assert_contains("<title>Hello, Kiln · Kiln blog</title>")
        .assert_contains(r#"<a href="/posts">Kiln blog</a>"#)
        // page HTML is inserted unescaped
        .assert_contains("<main><article>");
}

#[tokio::test]
async fn optional_relation_renders_when_present() {
    let client = app().await.client();
    client.get(posts::show::path("typed-paths")).await.assert_contains("edited by Ada Lovelace");
    client.get(posts::show::path("hello-kiln")).await.assert_not_contains("edited by");
}

#[tokio::test]
async fn optional_relation_loads_some_and_none() {
    let app = app().await;
    let db = &app.db;
    let posts = Post::query().preload(post::editor).all(db).await.unwrap();
    let editors: Vec<(&str, Option<&str>)> =
        posts.iter().map(|p| (p.slug.as_str(), p.editor().as_ref().map(|u| u.name.as_str()))).collect();
    assert_eq!(
        editors,
        [("hello-kiln", None), ("typed-paths", Some("Ada Lovelace")), ("no-n-plus-one", None), ("draft-post", None)]
    );
}

#[tokio::test]
async fn optional_relation_skips_the_query_when_every_key_is_empty() {
    let app = app().await;
    let db = &app.db;
    let before = db.query_count();
    let posts = Post::query().filter(post::editor_id.is_null()).preload(post::editor).all(db).await.unwrap();
    assert_eq!(posts.len(), 3);
    assert!(posts.iter().all(|p| p.editor().is_none()));
    assert_eq!(db.query_count() - before, 1, "only the posts query should run");
}

#[test]
fn parts_are_plain_methods_too() {
    let post = Post {
        id: Id::new(1),
        slug: "s".into(),
        title: "t".into(),
        body: "word ".repeat(500).into(),
        cover: None,
        status: PostStatus::Draft,
        author_id: Id::new(1),
        editor_id: None,
        published_at: None,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    };
    assert_eq!(post.reading_time(), "3 min read");
    assert!(post.is_draft());
}

#[tokio::test]
async fn agents_get_error_reports_as_markdown() {
    let client = app().await.client();
    let res = client.get("/nowhere").await;
    res.assert_status(404)
        .assert_contains("# 404 Not Found · No route (K1001)")
        .assert_contains("No route matches `GET /nowhere`.")
        .assert_contains("| GET | `/posts/{slug}` | `posts::show` |");
    assert_eq!(res.headers["content-type"], "text/markdown; charset=utf-8");
}

#[tokio::test]
async fn browsers_get_an_html_error_page_with_a_copy_button() {
    let client = app().await.client();
    let res = client.get_html(posts::show::path("nope")).await;
    res.assert_status(404)
        .assert_contains("<span class=\"status\">404</span>")
        .assert_contains("No <code>posts</code> row matched the query.")
        .assert_contains("<span class=\"action\">posts::show</span>")
        .assert_contains("Copy as Markdown")
        // The Markdown copy is embedded, escaped, for the button.
        .assert_contains("<textarea id=\"kiln-md\" hidden readonly># 404 Not Found · Record not found (K1003)");
    assert_eq!(res.headers["content-type"], "text/html; charset=utf-8");
}

#[tokio::test]
async fn wrong_method_is_405_with_a_report() {
    let client = app().await.client();
    client
        .request(kiln::__private::axum::http::Method::PUT, "/posts", None)
        .await
        .assert_status(405)
        .assert_contains("`/posts` exists, but not for `PUT`.");
}
