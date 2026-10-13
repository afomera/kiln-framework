//! Post CRUD, sign-in, policies, CSRF and forms.

use blog::{
    actions::{home, posts, sessions},
    records::*,
};
use kiln::{prelude::*, testing::TestApp};

#[kiln::test]
async fn signed_out_users_are_asked_to_sign_in(app: TestApp) -> Result {
    app.get(posts::new::path()).await.assert_status(401).assert_contains("Sign-in required (K1007)");
    Ok(())
}

#[kiln::test]
async fn author_creates_a_post(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    app.sign_in_as(&author)
        .post_form(posts::create::path(), &[("title", "Hello, World!"), ("body", "First post."), ("publish", "true")])
        .await
        .assert_redirect(&posts::show::path("hello-world"))
        .assert_flash("Post created.");

    let post = Post::find_by(post::slug.eq("hello-world")).one_or_404(&app.db).await?;
    assert_eq!((post.author_id, post.status), (author.id, PostStatus::Published));
    assert!(post.published_at.is_some());
    Ok(())
}

#[kiln::test]
async fn invalid_input_rerenders_the_form_with_errors(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    app.sign_in_as(&author)
        .post_form(posts::create::path(), &[("title", "Hi"), ("body", "")])
        .await
        .assert_status(422)
        .assert_contains("Title is too short (minimum is 3 characters)")
        .assert_contains("Body can&#x27;t be blank")
        // The user's input is kept.
        .assert_contains(r#"value="Hi""#);
    assert_eq!(Post::query().count(&app.db).await?, 0);
    Ok(())
}

#[kiln::test]
async fn titles_must_make_unique_slugs(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    app.create_with::<Post>(|p| p.slug = "taken-title".into()).await?;
    app.sign_in_as(&author)
        .post_form(posts::create::path(), &[("title", "Taken title"), ("body", "x")])
        .await
        .assert_status(422)
        .assert_contains("Title is already used by another post");
    Ok(())
}

#[kiln::test]
async fn unknown_form_fields_are_rejected(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    app.sign_in_as(&author)
        .post_form(posts::create::path(), &[("title", "Sneaky"), ("body", "x"), ("author_id", "1")])
        .await
        .assert_status(400)
        .assert_contains("unexpected form field `author_id`");
    Ok(())
}

#[kiln::test]
async fn forms_without_a_csrf_token_are_rejected(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    app.sign_in_as(&author)
        .post_form_without_csrf(posts::create::path(), &[("title", "Hello"), ("body", "x")])
        .await
        .assert_status(403)
        .assert_contains("Invalid CSRF token (K1006)");
    Ok(())
}

#[kiln::test]
async fn author_updates_their_post(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let author = User::find(post.author_id).one_or_404(&app.db).await?;
    let client = app.sign_in_as(&author);

    client.get(posts::edit::path(&post.slug)).await.assert_status(200).assert_contains(&post.title);
    client
        .patch_form(posts::update::path(&post.slug), &[("title", "Renamed"), ("body", "New body")])
        .await
        .assert_redirect(&posts::show::path(&post.slug))
        .assert_flash("Post updated.");

    let post = Post::find(post.id).one_or_404(&app.db).await?;
    assert_eq!((post.title.as_str(), post.status), ("Renamed", PostStatus::Draft));
    assert!(post.published_at.is_none(), "unchecking publish unpublishes");
    Ok(())
}

#[kiln::test]
async fn other_users_cannot_edit_or_delete(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let stranger = app.sign_in_as(&app.create::<User>().await?);
    stranger
        .get(posts::edit::path(&post.slug))
        .await
        .assert_status(403)
        .assert_contains("may not `Update` this `Post`");
    stranger
        .patch_form(posts::update::path(&post.slug), &[("title", "Mine now"), ("body", "x")])
        .await
        .assert_status(403);
    stranger.delete(posts::destroy::path(&post.slug)).await.assert_status(403);
    assert_eq!(Post::find(post.id).one_or_404(&app.db).await?.title, post.title);
    Ok(())
}

#[kiln::test]
async fn deleting_a_post_deletes_its_comments(app: TestApp) -> Result {
    let comment = app.create::<Comment>().await?;
    let post = Post::find(comment.post_id).one_or_404(&app.db).await?;
    let author = User::find(post.author_id).one_or_404(&app.db).await?;
    app.sign_in_as(&author)
        .delete(posts::destroy::path(&post.slug))
        .await
        .assert_redirect(&posts::index::path())
        .assert_flash("Post deleted.");
    assert_eq!(Post::query().count(&app.db).await?, 0);
    assert_eq!(Comment::query().count(&app.db).await?, 0);
    Ok(())
}

#[kiln::test]
async fn show_offers_edit_only_to_the_author(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let author = User::find(post.author_id).one_or_404(&app.db).await?;
    app.sign_in_as(&author).get(posts::show::path(&post.slug)).await.assert_contains(">Edit</a>");
    app.get(posts::show::path(&post.slug)).await.assert_not_contains(">Edit</a>");
    Ok(())
}

#[kiln::test]
async fn sign_in_and_out(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.client();
    client.get_html(sessions::new::path()).await.assert_status(200).assert_contains("Forgot your password?");
    client
        .post_form(sessions::create::path(), &[("email", &user.email), ("password", "password")])
        .await
        .assert_redirect(&home::index::path())
        .assert_flash(&format!("Signed in as {}.", user.name));
    client.get_html(posts::new::path()).await.assert_status(200).assert_contains("New post");
    client.delete(sessions::destroy::path()).await.assert_flash("Signed out.");
    client.get(posts::new::path()).await.assert_status(401);
    Ok(())
}

#[kiln::test]
async fn unknown_email_rerenders_sign_in(app: TestApp) -> Result {
    app.client()
        .post_form(sessions::create::path(), &[("email", "nobody@example.com"), ("password", "password")])
        .await
        .assert_status(422)
        .assert_contains("Email or password is incorrect");
    Ok(())
}

#[kiln::test]
async fn signed_out_browsers_are_sent_to_sign_in(app: TestApp) -> Result {
    app.client()
        .get_html(posts::new::path())
        .await
        .assert_redirect(&sessions::new::path())
        .assert_flash("Sign in to continue.");
    Ok(())
}

#[kiln::test]
async fn layouts_show_the_signed_in_user(app: TestApp) -> Result {
    let user = app.create_with::<User>(|u| u.name = "Ada Lovelace".into()).await?;
    app.sign_in_as(&user).get_html(posts::index::path()).await.assert_contains(">Ada Lovelace</a>");
    app.client()
        .get_html(posts::index::path())
        .await
        .assert_not_contains("Ada Lovelace")
        .assert_contains(">Sign in</a>");
    Ok(())
}

/// The layout's `current_user` reuses the user the action loaded with `Current<User>`.
#[kiln::test]
async fn current_user_is_loaded_once_per_page(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.sign_in_as(&user);
    client.get(posts::index::path()).await; // stores the sign-in

    // posts::new takes Current<User>; the layout reads current_user: one user query total.
    let before = app.db.query_count();
    client.get_html(posts::new::path()).await.assert_status(200);
    assert_eq!(app.db.query_count() - before, 1);

    // posts::index doesn't take the user; the layout loads it once (plus posts, authors, comments).
    app.create::<Post>().await?;
    let before = app.db.query_count();
    client.get_html(posts::index::path()).await.assert_status(200);
    assert_eq!(app.db.query_count() - before, 4);
    Ok(())
}

/// Redirects render nothing, so they don't load `current_user`: signing out is one delete.
#[kiln::test]
async fn redirects_skip_loading_current_user(app: TestApp) -> Result {
    let user = app.create::<User>().await?;
    let client = app.sign_in_as(&user);
    client.get(posts::index::path()).await; // stores the sign-in
    let before = app.db.query_count();
    client.delete(sessions::destroy::path()).await.assert_status(303);
    assert_eq!(app.db.query_count() - before, 1);
    Ok(())
}
