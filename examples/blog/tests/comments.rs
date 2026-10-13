//! Live comments: Turbo Stream responses, broadcasts and reliable streams.

use blog::{
    actions::{comments, posts},
    records::*,
};
use kiln::{prelude::*, testing::TestApp};

/// The page's `<turbo-stream-source>` URL.
fn stream_url(html: &str) -> String {
    let start = html.find("/kiln/streams/").expect("the page subscribes to a stream");
    let end = html[start..].find('"').expect("closing quote") + start;
    html[start..end].to_owned()
}

#[kiln::test]
async fn posting_a_comment_broadcasts_it(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let user = app.create_with::<User>(|u| u.name = "Grace".into()).await?;
    app.sign_in_as(&user)
        .post_form(comments::create::path(&post.slug), &[("body", "Nice post")])
        .await
        .assert_redirect(&posts::show::path(&post.slug))
        .assert_flash("Comment posted.");
    app.assert_broadcast(&post::stream_name(post.id), "<strong>Grace</strong>: Nice post").await?;
    Ok(())
}

#[kiln::test]
async fn turbo_gets_a_stream_that_appends_and_resets_the_form(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let user = app.create::<User>().await?;
    let res = app.sign_in_as(&user).turbo_post_form(comments::create::path(&post.slug), &[("body", "Streamed")]).await;
    res.assert_status(200)
        .assert_contains(r#"<turbo-stream action="append" target="comments"><template><li id="comment_"#)
        .assert_contains(
            r#"<turbo-stream action="replace" target="new_comment"><template><turbo-frame id="new_comment">"#,
        );
    assert_eq!(res.headers["content-type"], "text/vnd.turbo-stream.html");
    Ok(())
}

#[kiln::test]
async fn invalid_comments_rerender_the_frame(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let user = app.create::<User>().await?;
    app.sign_in_as(&user)
        .turbo_post_form(comments::create::path(&post.slug), &[("body", "")])
        .await
        .assert_status(422)
        .assert_contains(r#"<turbo-frame id="new_comment">"#)
        .assert_contains("Add a comment can&#x27;t be blank")
        .assert_not_contains("<header>");
    assert!(app.broadcasts(&post::stream_name(post.id)).await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn signed_out_users_cannot_comment(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    app.client().post_form(comments::create::path(&post.slug), &[("body", "Hi")]).await.assert_status(401);
    assert!(app.broadcasts(&post::stream_name(post.id)).await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn rolled_back_broadcasts_are_never_sent(app: TestApp) -> Result {
    let result = app
        .db
        .transaction(|tx| async move {
            tx.broadcast("rollback-test", TurboStream::new().refresh()).await?;
            Err::<(), _>(Error::NotFound)
        })
        .await;
    assert!(result.is_err());
    assert!(app.broadcasts("rollback-test").await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn pages_receive_broadcasts_live(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let url = stream_url(&app.get_html(posts::show::path(&post.slug)).await.text);
    let commenter = app.sign_in_as(&app.create::<User>().await?);
    let viewer = app.client();

    let (received, _) = tokio::join!(viewer.read_stream(&url, None, |text| text.contains("Live hello")), async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        commenter.post_form(comments::create::path(&post.slug), &[("body", "Live hello")]).await
    });
    assert!(received.starts_with("retry: 2000"), "{received}");
    assert!(received.contains("id: "), "{received}");
    assert!(received.contains(r#"data: <turbo-stream action="append" target="comments">"#), "{received}");
    assert!(received.contains("Live hello"), "{received}");
    Ok(())
}

#[kiln::test]
async fn reconnecting_pages_get_exactly_what_they_missed(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let url = stream_url(&app.get_html(posts::show::path(&post.slug)).await.text);
    let commenter = app.sign_in_as(&app.create::<User>().await?);
    for body in ["First", "Second", "Third"] {
        commenter.post_form(comments::create::path(&post.slug), &[("body", body)]).await.assert_status(303);
    }
    let first_id = app.db.stream_messages(&post::stream_name(post.id), 0).await?[0].0;

    // A browser that saw "First" reconnects with its id.
    let replay = app.client().read_stream(&url, Some(&first_id.to_string()), |t| t.contains("Third")).await;
    assert!(!replay.contains("First"), "{replay}");
    assert!(replay.contains("Second") && replay.contains("Third"), "{replay}");
    Ok(())
}

#[kiln::test]
async fn gaps_older_than_the_log_refresh_the_page(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let url = stream_url(&app.get_html(posts::show::path(&post.slug)).await.text);
    let commenter = app.sign_in_as(&app.create::<User>().await?);
    for body in ["One", "Two"] {
        commenter.post_form(comments::create::path(&post.slug), &[("body", body)]).await.assert_status(303);
    }
    // Simulate retention pruning the oldest message.
    let oldest = app.db.stream_messages(&post::stream_name(post.id), 0).await?[0].0;
    app.db.execute(&format!("DELETE FROM kiln_stream_messages WHERE id = {oldest}")).await?;

    let text = app.client().read_stream(&url, Some(&(oldest - 1).to_string()), |t| t.contains("refresh")).await;
    assert!(text.contains(r#"<turbo-stream action="refresh"></turbo-stream>"#), "{text}");
    Ok(())
}

#[kiln::test]
async fn stream_names_are_signed(app: TestApp) -> Result {
    app.get("/kiln/streams/cG9zdDox.forged").await.assert_status(404);
    Ok(())
}
