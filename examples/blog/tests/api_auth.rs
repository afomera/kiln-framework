//! API tokens on `pipeline = Api` routes (Bearer and git's HTTP Basic), scopes, expiry,
//! service secrets and rate limits. The settings pages are in `api_tokens.rs`.

use blog::{
    actions::{api, sign_ins},
    records::*,
};
use kiln::{
    auth,
    prelude::*,
    testing::{Method, TestApp, json},
    time::SignedDuration,
};

#[kiln::test]
async fn tokens_start_with_the_prefix_and_are_stored_hashed(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let issued = auth::create_api_token(&app.db, &user, "Laptop", &["read"], None).await?;
    let secret = issued.secret.expose();
    assert!(secret.starts_with("kiln_pat_") && secret.len() == "kiln_pat_".len() + 40, "{secret}");
    assert_eq!(issued.token.hint, format!("{}…", &secret[..13]));
    assert!(!format!("{issued:?}").contains(secret.as_str()), "Debug never prints the token");
    // Only a digest is kept: nothing stored equals or contains the token.
    app.db
        .execute(&format!(
            "UPDATE \"kiln_api_tokens\" SET \"name\" = 'plaintext!' WHERE \"token_digest\" = '{secret}' \
             OR \"hint\" LIKE '%{secret}%' OR \"scopes\" LIKE '%{secret}%'"
        ))
        .await?;
    assert_eq!(auth::api_tokens_of(&app.db, &user).await?[0].name, "Laptop");
    Ok(())
}

#[kiln::test]
async fn bearer_tokens_sign_in_on_api_routes(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.api_client(&user, &["read"]).await?;
    let res = client.get(api::me::show::path()).await;
    res.assert_status(200);
    assert_eq!(res.json()["email"], user.email);
    assert!(res.headers.get("set-cookie").is_none(), "token requests never get a session cookie");

    app.client().bearer("kiln_pat_nope").get(api::me::show::path()).await.assert_status(401);
    app.client().get(api::me::show::path()).await.assert_status(401);
    Ok(())
}

/// Browser routes use the cookie (and CSRF) only, so a leaked API token can't reach the
/// settings pages, e.g. to mint a token with more scopes.
#[kiln::test]
async fn browser_routes_ignore_tokens(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.api_client(&user, &["read", "write"]).await?;
    client.get(sign_ins::index::path()).await.assert_status(401);
    Ok(())
}

#[kiln::test]
async fn missing_scopes_are_403s(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let res = app.api_client(&user, &["write"]).await?.get(api::me::show::path()).await;
    res.assert_status(403);
    assert_eq!(res.json()["code"], "K1021");
    assert!(res.json()["detail"].as_str().unwrap().contains("`read`"));
    Ok(())
}

#[kiln::test]
async fn expired_and_revoked_tokens_stop_working(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let issued = auth::create_api_token(&app.db, &user, "CI", &["read"], Some(SignedDuration::from_hours(1))).await?;
    let client = app.client().bearer(issued.secret.expose());
    client.get(api::me::show::path()).await.assert_status(200);
    app.travel(SignedDuration::from_hours(2));
    client.get(api::me::show::path()).await.assert_status(401);
    assert!(auth::api_tokens_of(&app.db, &user).await?[0].expired);
    app.travel_back();

    let other = app.api_client(&user, &["read"]).await?;
    assert_eq!(auth::revoke_api_tokens(&app.db, &user).await?, 2);
    other.get(api::me::show::path()).await.assert_status(401);
    Ok(())
}

/// git sends credentials as HTTP Basic with the token as the password.
#[kiln::test]
async fn basic_auth_with_a_token_password_works_for_git(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let issued = auth::create_api_token(&app.db, &user, "git", &["read"], None).await?;
    let git = app.client().basic("ada", issued.secret.expose());
    git.get(api::me::show::path()).await.assert_status(200);
    // A staging site's own Basic password isn't taken for a token.
    app.client().basic("ada", "hunter2").get(api::me::show::path()).await.assert_status(401);
    Ok(())
}

/// Browsers resend saved Basic credentials on cross-site form posts, so Basic can't
/// authenticate form-encoded writes; JSON (which needs CORS) is fine.
#[kiln::test]
async fn basic_auth_refuses_form_posts(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let issued = auth::create_api_token(&app.db, &user, "git", &["write"], None).await?;
    let git = app.client().basic("ada", issued.secret.expose());
    let res = git.post_form_without_csrf(api::subscriptions::create::path(), &[("email", "a@example.com")]).await;
    res.assert_status(400);
    assert!(res.json()["detail"].as_str().unwrap().contains("Bearer"));
    git.send_json(Method::POST, api::subscriptions::create::path(), &json!({ "email": "a@example.com" }))
        .await
        .assert_status(200);
    Ok(())
}

#[kiln::test]
async fn shared_secrets_compare_bearer_headers(app: TestApp) -> Result {
    let _ = &app;
    let mut headers = HeaderMap::new();
    assert!(!auth::bearer_matches(&headers, "s3cret"));
    headers.insert("authorization", "Bearer s3cret".parse().unwrap());
    assert!(auth::bearer_matches(&headers, "s3cret"));
    assert!(!auth::bearer_matches(&headers, "s3cre"));
    assert!(!auth::bearer_matches(&headers, ""), "an unset secret never matches");
    headers.insert("authorization", "Basic s3cret".parse().unwrap());
    assert!(!auth::bearer_matches(&headers, "s3cret"));
    Ok(())
}

/// The top of an hour, so a test's requests all land in one rate-limit window.
fn on_the_hour() -> Timestamp {
    Timestamp::from_jiff(kiln::time::Timestamp::from_second(1_800_000_000).unwrap())
}

#[kiln::test]
async fn rate_limits_answer_429_with_retry_after(app: TestApp) -> Result {
    app.travel_to(on_the_hour());
    let client = app.client().header("x-forwarded-for", "203.0.113.9");
    for n in 0..10 {
        client
            .send_json(
                Method::POST,
                api::subscriptions::create::path(),
                &json!({ "email": format!("r{n}@example.com") }),
            )
            .await
            .assert_status(200);
    }
    let res =
        client.send_json(Method::POST, api::subscriptions::create::path(), &json!({ "email": "r@example.com" })).await;
    res.assert_status(429);
    assert_eq!(res.json()["code"], "K1022");
    let retry_after: i64 = res.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((3500..=3600).contains(&retry_after), "the window resets at the next hour: {retry_after}");

    // Counted per IP address, per window.
    app.client()
        .header("x-forwarded-for", "198.51.100.4")
        .send_json(Method::POST, api::subscriptions::create::path(), &json!({ "email": "s@example.com" }))
        .await
        .assert_status(200);
    app.travel(SignedDuration::from_secs(retry_after));
    client
        .send_json(Method::POST, api::subscriptions::create::path(), &json!({ "email": "r@example.com" }))
        .await
        .assert_status(200);
    Ok(())
}

/// With `Current<..>`, the limit is per signed-in record, whichever token or address.
#[kiln::test]
async fn rate_limits_count_per_user_when_signed_in(app: TestApp) -> Result {
    app.travel_to(on_the_hour());
    let (ada, bob) = (app.create::<User>().await?, app.create::<User>().await?);
    let (laptop, ci) = (app.api_client(&ada, &["read"]).await?, app.api_client(&ada, &["read"]).await?);
    let bob = app.api_client(&bob, &["read"]).await?;
    for _ in 0..300 {
        laptop.get(api::me::show::path()).await.assert_status(200);
        ci.get(api::me::show::path()).await.assert_status(200);
    }
    ci.get(api::me::show::path()).await.assert_status(429);
    bob.get(api::me::show::path()).await.assert_status(200);
    Ok(())
}
