use kiln::prelude::*;

use crate::{records::User, views::api_tokens::Index};

/// The user's API tokens, and a form to create one.
#[action(GET "/settings/api_tokens")]
pub async fn index(ctx: Ctx, user: Current<User>) -> Result<Index> {
    Ok(Index { tokens: kiln::auth::api_tokens_of(&ctx.db, &*user).await?, form: FormState::new() })
}
