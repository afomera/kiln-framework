//! The JSON API: `pipeline = Api` routes.

use blog::{actions::api, records::*};
use kiln::{
    prelude::*,
    testing::{TestApp, json},
};

#[kiln::test]
async fn lists_published_posts_as_json(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    app.create_with::<Post>(|p| p.status = PostStatus::Draft).await?;
    let res = app.get(api::posts::index::path()).await;
    res.assert_status(200);
    assert_eq!(res.headers["content-type"], "application/json");
    let body = res.json();
    let posts = body.as_array().expect("an array");
    assert_eq!(posts.len(), 1, "drafts are excluded");
    assert_eq!(posts[0]["slug"], post.slug);
    assert_eq!(posts[0]["url"], format!("/posts/{}", post.slug));
    assert!(posts[0]["author"].is_string());
    assert!(!res.text.contains("@example.com"), "author emails stay private");
    Ok(())
}

#[kiln::test]
async fn missing_posts_are_json_problems(app: TestApp) -> Result {
    let res = app.client().get_html(api::posts::show::path("nope")).await;
    res.assert_status(404);
    assert_eq!(res.headers["content-type"], "application/problem+json", "API errors are JSON even for browsers");
    let body = res.json();
    assert_eq!(body["code"], "K1003");
    assert_eq!(body["status"], 404);
    Ok(())
}

#[kiln::test]
async fn subscribes_with_json(app: TestApp) -> Result {
    let res =
        app.client().post_json(api::subscriptions::create::path(), &json!({ "email": "reader@example.com" })).await;
    res.assert_status(200);
    assert_eq!(res.json()["email"], "reader@example.com");
    assert_eq!(Subscription::query().count(&app.db).await?, 1);
    Ok(())
}

#[kiln::test]
async fn invalid_json_input_lists_field_errors(app: TestApp) -> Result {
    let client = app.client();
    let res = client.post_json(api::subscriptions::create::path(), &json!({ "email": "not-an-email" })).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"]["email"], json!(["must be an email address"]));

    client.post_json(api::subscriptions::create::path(), &json!({ "email": "a@example.com" })).await.assert_status(200);
    let res = client.post_json(api::subscriptions::create::path(), &json!({ "email": "a@example.com" })).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"]["email"], json!(["is already subscribed"]));
    Ok(())
}

#[kiln::test]
async fn unknown_json_fields_are_rejected(app: TestApp) -> Result {
    let res = app
        .client()
        .post_json(api::subscriptions::create::path(), &json!({ "email": "a@example.com", "admin": true }))
        .await;
    res.assert_status(400);
    assert!(res.json()["detail"].as_str().unwrap().contains("unexpected form field `admin`"));
    Ok(())
}

/// API routes skip CSRF, so they must not trust the session cookie either.
#[kiln::test]
async fn api_routes_ignore_the_session_cookie(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let signed_in = app.sign_in_as(&user);
    let res = signed_in.post_json(api::subscriptions::create::path(), &json!({ "email": "c@example.com" })).await;
    res.assert_status(200);
    assert!(res.headers.get("set-cookie").is_none(), "API responses never set the session cookie");
    Ok(())
}
