use kiln::prelude::*;

use crate::{actions::api_tokens, records::User};

/// Revokes a token; it stops working at once. Ids of other people's tokens are ignored.
#[action(DELETE "/settings/api_tokens/{id}")]
pub async fn destroy(ctx: Ctx, user: Current<User>, id: Param<i64>) -> Result<Redirect> {
    let message =
        if kiln::auth::revoke_api_token(&ctx.db, &*user, *id).await? { "Token revoked." } else { "Already revoked." };
    Ok(Redirect::to(api_tokens::index::path()).notice(message))
}
