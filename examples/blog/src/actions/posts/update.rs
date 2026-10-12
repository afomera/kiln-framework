use kiln::{policy::Update, prelude::*};

use crate::{actions::posts, forms::PostForm, ops, records::*, views::posts::Edit};

#[action(PATCH "/posts/{slug}")]
pub async fn update(ctx: Ctx, user: Current<User>, slug: Param<String>, form: Form<PostForm>) -> Result<Redirect> {
    let post = Post::find_by(post::slug.eq(&*slug)).one_or_404(&ctx.db).await?;
    let post = user.authorize::<Update, _>(post)?;
    let input = form.validate().or_render(|form| Edit { form, post: (*post).clone() })?;
    let post = ops::update_post(&ctx, post, input).await?;
    Ok(Redirect::to(posts::show::path(&post.slug)).notice("Post updated."))
}
