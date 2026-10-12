use kiln::prelude::*;

use crate::records::*;

/// Sample content for development. Does nothing if there are already users.
pub async fn seed(db: Db) -> Result {
    let db = &db;
    if User::query().exists(db).await? {
        return Ok(());
    }
    let ada = User::insert(NewUser { name: "Ada Lovelace".into(), email: "ada@example.com".into() }).run(db).await?;
    let grace =
        User::insert(NewUser { name: "Grace Hopper".into(), email: "grace@example.com".into() }).run(db).await?;

    let posts = [
        ("hello-kiln", "Hello, Kiln", "The compiler is the reviewer.", PostStatus::Published, &ada, None),
        ("typed-paths", "Typed paths", "Broken links don't compile.", PostStatus::Published, &grace, Some(ada.id)),
        ("no-n-plus-one", "No N+1", "Relations exist only when preloaded.", PostStatus::Published, &ada, None),
        ("draft-post", "Draft post", "Not ready yet.", PostStatus::Draft, &grace, None),
    ];
    for (slug, title, body, status, author, editor_id) in posts {
        let post = Post::insert(NewPost {
            slug: slug.into(),
            title: title.into(),
            body: body.into(),
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
