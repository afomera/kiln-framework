use kiln::{policy::Update, prelude::*};

use crate::{records::*, views::posts::Show};

#[action(GET "/posts/{slug}")]
pub async fn show(ctx: Ctx, user: Option<Current<User>>, slug: Param<String>) -> Result<Show> {
    let post = Post::find_by(post::slug.eq(&*slug))
        .preload(post::author)
        .preload(post::editor)
        .preload(post::comments.preload(comment::author))
        .one_or_404(&ctx.db)
        .await?;

    let by_line = format!("{} · {} comments", post.author().name, post.comments().len());
    let can_edit = user.is_some_and(|u| u.can::<Update, _>(&post));
    Ok(Show { post, by_line, can_edit, comment_form: FormState::new() })
}
