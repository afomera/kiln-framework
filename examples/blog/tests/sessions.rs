use blog::{
    actions::{home, sessions},
    records::*,
};
use kiln::{prelude::*, testing::TestApp};

#[kiln::test]
async fn sign_in_and_out(app: TestApp) -> Result {
    // Factories give every user the password `password`.
    let user = app.create::<User>().await?;
    let client = app.client();
    // Password managers fill sign-in forms from these hints.
    client
        .get_html(sessions::new::path())
        .await
        .assert_contains("autocomplete=\"current-password\"")
        .assert_contains("autocomplete=\"email\"");
    client
        .post_form(sessions::create::path(), &[("email", &user.email), ("password", "password")])
        .await
        .assert_redirect(&home::index::path())
        .assert_flash(&format!("Signed in as {}.", user.name));
    client.delete(sessions::destroy::path()).await.assert_flash("Signed out.");
    Ok(())
}

#[kiln::test]
async fn unknown_email_rerenders_the_form(app: TestApp) -> Result {
    app.client()
        .post_form(sessions::create::path(), &[("email", "nobody@example.com"), ("password", "password")])
        .await
        .assert_status(422)
        .assert_contains("or password is incorrect");
    Ok(())
}
