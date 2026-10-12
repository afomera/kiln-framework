use kiln::{policy::Update, prelude::*};

use crate::{forms::PostForm, records::*, views::posts::Edit};

#[action(GET "/posts/{slug}/edit")]
pub async fn edit(ctx: Ctx, user: Current<User>, slug: Param<String>) -> Result<Edit> {
    let post = Post::find_by(post::slug.eq(&*slug)).one_or_404(&ctx.db).await?;
    let post = user.authorize::<Update, _>(post)?;
    Ok(Edit { form: FormState::from_input(&PostForm::from_record(&post)), post: post.into_inner() })
}
