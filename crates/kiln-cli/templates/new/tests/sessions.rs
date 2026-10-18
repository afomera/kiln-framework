use @@crate@@::{actions::sessions, records::*};
use kiln::{prelude::*, testing::TestApp};

#[kiln::test]
async fn sign_in_page_renders(app: TestApp) -> Result {
    app.get_html(sessions::new::path()).await.assert_status(200).assert_contains("Sign in");
    Ok(())
}

#[kiln::test]
async fn sign_in_and_out(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.client();
    client
        .post_form(sessions::create::path(), &[("email", &user.email)])
        .await
        .assert_redirect("/")
        .assert_flash(&format!("Signed in as {}.", user.name));
    client.delete(sessions::destroy::path()).await.assert_flash("Signed out.");
    Ok(())
}

#[kiln::test]
async fn unknown_email_rerenders_the_form_without_saying_so(app: TestApp) -> Result {
    app.client()
        .post_form(sessions::create::path(), &[("email", "nobody@example.com")])
        .await
        .assert_status(422)
        .assert_contains("can&#x27;t be used to sign in");
    Ok(())
}
