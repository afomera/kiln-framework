//! Comment notifications: mail queued with the comment, rendered from templates.

use blog::{actions::comments, mail::CommentNotification, records::*};
use kiln::{prelude::*, testing::TestApp};

#[kiln::test]
async fn commenting_emails_the_post_author(app: TestApp) -> Result {
    let author = app.create_with::<User>(|u| u.email = "ada@example.com".into()).await?;
    let post = app.create_with::<Post>(|p| (p.author_id, p.title) = (author.id, "Engines".into())).await?;
    let reader = app.create_with::<User>(|u| u.name = "Grace".into()).await?;
    app.sign_in_as(&reader).post_form(comments::create::path(&post.slug), &[("body", "Lovely")]).await.assert_status(303);

    assert!(app.outbox().await?.is_empty(), "mail goes out from a job, not the request");
    app.perform_enqueued_jobs().await?;
    let mail = app.assert_mailed("ada@example.com", "Grace commented on “Engines”").await?;
    assert!(mail.html.contains("<blockquote>Lovely</blockquote>"), "{}", mail.html);
    assert!(mail.html.contains("Sent by the Kiln blog"), "the mail layout wraps the body");
    assert!(mail.html.contains(&format!("http://localhost:3000/posts/{}", post.slug)), "links are absolute");
    assert!(mail.text.as_deref().is_some_and(|t| t.contains("Lovely")), "has a text part");
    Ok(())
}

#[kiln::test]
async fn authors_are_not_emailed_about_their_own_comments(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let author = User::find(post.author_id).one_or_404(&app.db).await?;
    app.sign_in_as(&author).post_form(comments::create::path(&post.slug), &[("body", "Me again")]).await.assert_status(303);
    app.assert_enqueued::<kiln::mail::Deliver>(0).await?;
    Ok(())
}

#[kiln::test]
async fn mail_renders_without_sending(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let mail = CommentNotification { to: "x@example.com".into(), post, commenter: "Lin".into(), body: "<b>hi</b>".into() };
    let message = mail.message()?;
    assert!(message.html.contains("&lt;b&gt;hi&lt;&#x2f;b&gt;"), "bodies are escaped: {}", message.html);
    assert_eq!(message.from, "app@localhost");
    Ok(())
}
