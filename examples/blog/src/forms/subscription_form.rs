use kiln::{
    form::{CheckWith, Errors},
    prelude::*,
};

use crate::records::{Subscription, subscription};

#[derive(Form, Debug)]
pub struct SubscriptionForm {
    #[validate(email)]
    pub email: String,
}

impl CheckWith for SubscriptionForm {
    async fn check(&self, ctx: &Ctx, errors: &mut Errors) -> Result {
        if Subscription::find_by(subscription::email.eq(&self.email)).exists(&ctx.db).await? {
            errors.add("email", "is already subscribed");
        }
        Ok(())
    }
}
