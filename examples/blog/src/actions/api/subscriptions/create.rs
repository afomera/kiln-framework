use kiln::prelude::*;

use crate::{forms::SubscriptionForm, records::*, views::api::SubscriptionJson};

/// Newsletter signup. Accepts JSON (`{"email": ".."}`) or a form post. Ten an hour per IP
/// address, so it can't be used to flood the list.
#[action(POST "/api/subscriptions", pipeline = Api)]
pub async fn create(ctx: Ctx, limit: RateLimit, form: Form<SubscriptionForm>) -> Result<Json<SubscriptionJson>> {
    limit.check("subscribe", 10, kiln::time::SignedDuration::from_hours(1)).await?;
    let input = form.validate_with(&ctx).await?;
    let subscription = Subscription::insert(NewSubscription { email: input.into_inner().email }).run(&ctx.db).await?;
    Ok(Json(SubscriptionJson { email: subscription.email }))
}
