use kiln::prelude::*;

use crate::{
    actions::sign_ins,
    forms::ChangePasswordForm,
    ops::{self, PasswordChange},
    records::User,
    views::password_settings::Edit,
};

/// Changes the password and signs out every other device; this browser stays signed in.
#[action(PATCH "/settings/password")]
pub async fn update(ctx: Ctx, session: Session, user: Current<User>, form: Form<ChangePasswordForm>) -> Result<Redirect> {
    let input = form.validate().or_render(|form| Edit { form })?;
    let state = FormState::from_input(&*input);
    let changed = match ops::change_password(&ctx, &user, input).await? {
        PasswordChange::Changed(changed) => changed,
        PasswordChange::WrongCurrentPassword => {
            return Err(Error::unprocessable(&Edit { form: state.with_error("current_password", "is incorrect") }));
        }
        PasswordChange::ConfirmationMismatch => {
            return Err(Error::unprocessable(&Edit { form: state.with_error("password_confirmation", "doesn't match") }));
        }
    };
    let others = session.sign_out_others(&ctx.db, &changed).await?;
    let devices = match others {
        0 => String::new(),
        1 => " We signed out your other device.".to_owned(),
        n => format!(" We signed out your {n} other devices."),
    };
    Ok(Redirect::to(sign_ins::index::path()).notice(format!("Password changed.{devices}")))
}
