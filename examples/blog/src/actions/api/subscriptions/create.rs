use kiln::prelude::*;

use crate::{forms::SubscriptionForm, records::*, views::api::SubscriptionJson};

/// Newsletter signup. Accepts JSON (`{"email": ".."}`) or a form post.
#[action(POST "/api/subscriptions", pipeline = Api)]
pub async fn create(ctx: Ctx, form: Form<SubscriptionForm>) -> Result<Json<SubscriptionJson>> {
    let input = form.validate_with(&ctx).await?;
    let subscription = Subscription::insert(NewSubscription { email: input.into_inner().email }).run(&ctx.db).await?;
    Ok(Json(SubscriptionJson { email: subscription.email }))
}
