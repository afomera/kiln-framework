use kiln::prelude::*;

use crate::ops;

#[action(GET "/confirm/{token}")]
pub async fn show(ctx: Ctx, token: Param<String>) -> Result<Redirect> {
    Ok(match ops::confirm_email(&ctx, &token).await? {
        Some(_) => Redirect::to("/").notice("Email confirmed. Thanks!"),
        None => Redirect::to("/").alert("That confirmation link is invalid or has expired."),
    })
}
