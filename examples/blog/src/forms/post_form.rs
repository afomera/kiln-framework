use kiln::{
    form::{CheckWith, Errors},
    prelude::*,
};

use crate::records::{NewPost, Post, PostStatus, User, post};

#[derive(Form, Debug)]
pub struct PostForm {
    #[validate(length(min = 3, max = 200))]
    pub title: String,
    /// The rich text editor. `length` counts the words people see, not the markup.
    #[validate(length(min = 1))]
    pub body: RichText,
    #[validate(file(max_size = "5MB", accept = "image/*"))]
    pub cover: Option<Attachment>,
    pub publish: bool,
}

impl PostForm {
    /// Prefills the edit form.
    pub fn from_record(post: &Post) -> Self {
        Self {
            title: post.title.clone(),
            body: post.body.clone(),
            cover: post.cover.clone(),
            publish: post.status == PostStatus::Published,
        }
    }

    /// The insert for a new post by `author`. The slug comes from the title.
    pub fn into_new(self, author: &User) -> NewPost {
        NewPost {
            slug: slugify(&self.title),
            status: status(self.publish),
            published_at: self.publish.then(Timestamp::now),
            title: self.title,
            body: self.body,
            cover: self.cover,
            author_id: author.id,
            editor_id: None,
        }
    }

    /// The update for `post`. The slug is kept so links don't break.
    pub fn apply(self, post: &Post, update: kiln::Update<Post>) -> kiln::Update<Post> {
        let update = update
            .set(post::title, self.title)
            .set(post::body, self.body)
            .set(post::cover, self.cover)
            .set(post::status, status(self.publish));
        match (self.publish, post.published_at) {
            (true, None) => update.set(post::published_at, Timestamp::now()),
            (false, Some(_)) => update.set(post::published_at, None::<Timestamp>),
            _ => update,
        }
    }
}

/// New posts need a title that makes a unique slug.
impl CheckWith for PostForm {
    async fn check(&self, ctx: &Ctx, errors: &mut Errors) -> Result {
        let slug = slugify(&self.title);
        if slug.is_empty() || slug == "new" {
            errors.add("title", "needs letters or numbers (and can't be just \"new\")");
        } else if Post::find_by(post::slug.eq(slug)).exists(&ctx.db).await? {
            errors.add("title", "is already used by another post");
        }
        Ok(())
    }
}

fn status(publish: bool) -> PostStatus {
    if publish { PostStatus::Published } else { PostStatus::Draft }
}

/// `"Hello, Kiln!"` → `"hello-kiln"`.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}
