// `NewPost::new(PostRequired { .. })` defaults the optional fields, but a required one
// can't be left out. A record whose fields all have defaults gets `Default` instead.
use blog::records::{NewPost, PostRequired};
use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "tags")]
pub struct Tag {
    pub id: Id<Tag>,
    pub name: Option<String>,
    #[default(3)]
    pub weight: i64,
    #[default]
    pub hidden: bool,
}

fn main() {
    let tag = NewTag::default();
    assert_eq!((tag.name, tag.weight, tag.hidden), (None, 3, false));

    let _post = NewPost { editor_id: None, ..NewPost::new(PostRequired { slug: "s".into(), title: "T".into() }) };
}
