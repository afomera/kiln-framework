use @@krate@@::{
    actions::{api, api_tokens},
    records::*,
};
use kiln::{prelude::*, testing::TestApp};

/// The token on the "Token created" page.
fn secret_in(page: &str) -> String {
    let start = page.find(kiln::auth::api_token_prefix()).expect("the page shows the token");
    page[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
}

#[kiln::test]
async fn create_use_and_revoke_a_token(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let browser = app.sign_in_as(&user);
    let page = browser
        .post_form(api_tokens::create::path(), &[("name", "Laptop"), ("scopes", "read"), ("expires_in_days", "30")])
        .await;
    page.assert_status(200).assert_contains("won't be shown again");
    let secret = secret_in(&page.text);

    // The list shows the token's name and hint, never the token.
    browser.get_html(api_tokens::index::path()).await.assert_contains("Laptop").assert_not_contains(&secret);

    let api = app.client().bearer(&secret);
    let me = api.get(api::me::show::path()).await;
    me.assert_status(200);
    assert_eq!(me.json()["email"], user.email);

    let token = &kiln::auth::api_tokens_of(&app.db, &user).await?[0];
    assert!(token.last_used_at.is_some());
    browser.delete(api_tokens::destroy::path(token.id)).await.assert_flash("Token revoked.");
    api.get(api::me::show::path()).await.assert_status(401);
    Ok(())
}

#[kiln::test]
async fn scopes_are_checked(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    app.api_client(&user, &[]).await?.get(api::me::show::path()).await.assert_status(403);
    app.api_client(&user, &["read"]).await?.get(api::me::show::path()).await.assert_status(200);
    Ok(())
}

#[kiln::test]
async fn unknown_scopes_rerender_the_form(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    app.sign_in_as(&user)
        .post_form(api_tokens::create::path(), &[("name", "CI"), ("scopes", "everything"), ("expires_in_days", "")])
        .await
        .assert_status(422)
        .assert_contains("unknown scope");
    assert!(kiln::auth::api_tokens_of(&app.db, &user).await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn other_peoples_tokens_cant_be_revoked(app: TestApp) -> Result {
    let (owner, other) = (app.create::<User>().await?, app.create::<User>().await?);
    let issued = kiln::auth::create_api_token(&app.db, &owner, "Laptop", &["read"], None).await?;
    app.sign_in_as(&other).delete(api_tokens::destroy::path(issued.token.id)).await.assert_flash("Already revoked.");
    app.client().bearer(issued.secret.expose()).get(api::me::show::path()).await.assert_status(200);
    Ok(())
}
