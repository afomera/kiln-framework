use kiln::{policy::Create, prelude::*, turbo::StreamOrRedirect};

use crate::{
    actions::posts,
    forms::CommentForm,
    ops,
    records::*,
    views::comments::{Compose, Item},
};

#[action(POST "/posts/{slug}/comments")]
pub async fn create(
    ctx: Ctx,
    user: Current<User>,
    turbo: Turbo,
    slug: Param<String>,
    form: Form<CommentForm>,
) -> Result<StreamOrRedirect> {
    let post = Post::find_by(post::slug.eq(&*slug)).one_or_404(&ctx.db).await?;
    let input = form.validate().or_render(|form| Compose { form, post_slug: post.slug.clone() })?;
    let new = user.authorize::<Create, _>(input.into_inner().into_new(&post, &user))?;
    let comment = ops::create_comment(&ctx, new).await?;
    // Turbo: add the comment and reset the form in place. Everyone else gets the broadcast.
    Ok(turbo.stream_or_redirect(
        TurboStream::new()
            .append("comments", Item { comment })
            .replace("new_comment", Compose { form: FormState::new(), post_slug: post.slug.clone() }),
        Redirect::to(posts::show::path(&post.slug)).notice("Comment posted."),
    ))
}
