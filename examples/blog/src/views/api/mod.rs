//! JSON shapes for API responses. Explicit structs keep internal columns (like emails)
//! out of responses.

use kiln::{Loaded, Timestamp};
use serde::Serialize;

use crate::{actions::posts, records::*};

#[derive(Serialize)]
pub struct PostJson {
    pub slug: String,
    pub title: String,
    pub body: String,
    pub author: String,
    pub published_at: Option<Timestamp>,
    pub url: String,
}

impl From<&Loaded<Post, (post::author,)>> for PostJson {
    fn from(post: &Loaded<Post, (post::author,)>) -> Self {
        Self {
            slug: post.slug.clone(),
            title: post.title.clone(),
            body: post.body.clone(),
            author: post.author().name.clone(),
            published_at: post.published_at,
            url: posts::show::path(&post.slug),
        }
    }
}

#[derive(Serialize)]
pub struct SubscriptionJson {
    pub email: String,
}
