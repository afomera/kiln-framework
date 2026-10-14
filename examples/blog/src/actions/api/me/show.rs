use kiln::{prelude::*, time::SignedDuration};

use crate::{records::User, views::current_user::CurrentUser};

/// Who a token belongs to, for API clients checking theirs:
/// `curl -H "Authorization: Bearer $TOKEN" localhost:4000/api/me`.
#[action(GET "/api/me", pipeline = Api)]
pub async fn show(user: Current<User>, limit: RateLimit) -> Result<Json<CurrentUser>> {
    limit.check("api", 600, SignedDuration::from_mins(1)).await?;
    user.require_scope("read")?;
    Ok(Json(CurrentUser::of(&user)))
}
