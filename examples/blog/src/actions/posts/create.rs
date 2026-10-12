use kiln::{policy::Create, prelude::*};

use crate::{actions::posts, forms::PostForm, ops, records::User, views::posts::New};

#[action(POST "/posts")]
pub async fn create(ctx: Ctx, user: Current<User>, form: Form<PostForm>) -> Result<Redirect> {
    let input = form.validate_with(&ctx).await.or_render(|form| New { form })?;
    let new = user.authorize::<Create, _>(input.into_inner().into_new(&user))?;
    let post = ops::create_post(&ctx, new).await?;
    Ok(Redirect::to(posts::show::path(&post.slug)).notice("Post created."))
}
