use kiln::prelude::*;

use crate::records::*;

/// Sample content for development. Does nothing if there are already users.
pub async fn seed(db: Db) -> Result {
    let db = &db;
    if User::query().exists(db).await? {
        return Ok(());
    }
    let ada = User::insert(NewUser {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        password_digest: kiln::auth::hash_password("password")?,
        confirmed_at: Some(Timestamp::now()),
    })
    .run(db)
    .await?;
    let grace = User::insert(NewUser {
        name: "Grace Hopper".into(),
        email: "grace@example.com".into(),
        password_digest: kiln::auth::hash_password("password")?,
        confirmed_at: Some(Timestamp::now()),
    })
    .run(db)
    .await?;

    let posts = [
        (
            "hello-kiln",
            "Hello, Kiln",
            "<h2>The compiler is the reviewer</h2><p>Kiln turns conventions into <strong>types</strong>, so mistakes fail to compile:</p><ul><li>N+1 queries</li><li>Unauthorized writes</li><li>Broken links</li></ul><blockquote><p>If it compiles, it follows the rules.</p></blockquote>",
            PostStatus::Published,
            &ada,
            None,
        ),
        (
            "typed-paths",
            "Typed paths",
            "<p>Every action gets a typed path helper, like <code>posts::show::path(&amp;slug)</code>. Rename a route and every caller stops compiling.</p>",
            PostStatus::Published,
            &grace,
            Some(ada.id),
        ),
        (
            "no-n-plus-one",
            "No N+1",
            "<p>Relations exist only when <em>preloaded</em>. Preloading changes the type, so <code>post.comments()</code> on a post without them is a compile error.</p>",
            PostStatus::Published,
            &ada,
            None,
        ),
        ("draft-post", "Draft post", "Not ready yet.", PostStatus::Draft, &grace, None),
    ];
    for (slug, title, body, status, author, editor_id) in posts {
        let post = Post::insert(NewPost {
            slug: slug.into(),
            title: title.into(),
            body: body.into(),
            cover: None,
            status,
            author_id: author.id,
            editor_id,
            published_at: (status == PostStatus::Published).then(Timestamp::now),
        })
        .run(db)
        .await?;

        for (commenter, text) in [(&grace, "Nice."), (&ada, "Thanks!")] {
            Comment::insert(NewComment { post_id: post.id, author_id: commenter.id, body: text.into() })
                .run(db)
                .await?;
        }
    }
    Ok(())
}
