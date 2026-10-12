//! Default records for tests: `app.create::<Post>()`. In the library (not `tests/`) so
//! factories can implement kiln's `Factory` trait for the app's records.

use kiln::{
    prelude::*,
    testing::{Factory, TestApp},
};

use crate::records::*;

impl Factory for User {
    async fn build(_app: &TestApp, n: u64) -> Result<NewUser> {
        Ok(NewUser { name: format!("User {n}"), email: format!("user{n}@example.com") })
    }
}

impl Factory for Post {
    async fn build(app: &TestApp, n: u64) -> Result<NewPost> {
        let author = app.create::<User>().await?;
        Ok(NewPost {
            slug: format!("post-{n}"),
            title: format!("Post {n}"),
            body: "Some words.".into(),
            status: PostStatus::Published,
            author_id: author.id,
            editor_id: None,
            published_at: Some(Timestamp::now()),
        })
    }
}

impl Factory for Comment {
    async fn build(app: &TestApp, n: u64) -> Result<NewComment> {
        let post = app.create::<Post>().await?;
        let author = app.create::<User>().await?;
        Ok(NewComment { post_id: post.id, author_id: author.id, body: format!("Comment {n}") })
    }
}

impl Factory for Bookmark {
    async fn build(app: &TestApp, n: u64) -> Result<NewBookmark> {
        let owner = app.create::<User>().await?;
        Ok(NewBookmark {
            title: format!("Title {n}"),
            url: format!("Url {n}"),
            notes: None,
            rating: None,
            pinned: false,
            owner_id: owner.id,
        })
    }
}

impl Factory for Subscription {
    async fn build(_app: &TestApp, n: u64) -> Result<NewSubscription> {
        Ok(NewSubscription { email: format!("Email {n}") })
    }
}
