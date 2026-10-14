use kiln::{prelude::*, time::SignedDuration};

use crate::{
    forms::ApiTokenForm,
    records::User,
    views::api_tokens::{Created, Index},
};

/// Creates a token and shows it, the only time it can be. The page is rendered rather than
/// redirected to, so the token is never stored anywhere, not even in the session cookie.
#[action(POST "/settings/api_tokens")]
pub async fn create(ctx: Ctx, user: Current<User>, form: Form<ApiTokenForm>) -> Result<Created> {
    let tokens = kiln::auth::api_tokens_of(&ctx.db, &*user).await?;
    let input = form.validate().or_render(|form| Index { tokens: tokens.clone(), form })?;
    let scopes = match input.scope_list() {
        Ok(scopes) => scopes,
        Err(message) => {
            let form = FormState::from_input(&*input).with_error("scopes", message);
            return Err(Error::unprocessable(&Index { tokens, form }));
        }
    };
    let expires_in = input.expires_in_days.map(|days| SignedDuration::from_hours(24 * days));
    let issued = kiln::auth::create_api_token(&ctx.db, &*user, &input.name, &scopes, expires_in).await?;
    Ok(Created { token: issued.token, secret: issued.secret.expose().clone() })
}
