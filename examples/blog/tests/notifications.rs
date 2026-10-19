//! Notifications: one event, delivered to the inbox and by email.

use blog::{
    actions::{comments, notifications},
    notifications::CommentPosted,
    records::*,
};
use kiln::{prelude::*, testing::TestApp, time::SignedDuration};

async fn comment_on_ada_post(app: &TestApp) -> Result<(User, Post)> {
    let ada = app.create_with::<User>(|u| u.email = "ada@example.com".into()).await?;
    let post = app.create_with::<Post>(|p| (p.author_id, p.title) = (ada.id, "Engines".into())).await?;
    let grace = app.create_with::<User>(|u| u.name = "Grace".into()).await?;
    app.sign_in_as(&grace)
        .post_form(comments::create::path(&post.slug), &[("body", "Lovely")])
        .await
        .assert_status(303);
    Ok((ada, post))
}

#[kiln::test]
async fn a_comment_lands_in_the_inbox_and_the_mailbox(app: TestApp) -> Result {
    let (ada, post) = comment_on_ada_post(&app).await?;
    assert_eq!(app.db.unread_notifications(&ada).await?, 0, "delivered by a job");
    app.assert_notified::<CommentPosted>(&ada, 1).await?;
    app.perform_enqueued_jobs().await?;
    app.assert_notified::<CommentPosted>(&ada, 0).await?;

    let inbox = app.db.inbox(&ada, 10).await?;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].title, "Grace commented on “Engines”");
    assert_eq!(inbox[0].kind, "CommentPosted");
    assert_eq!(inbox[0].url.as_deref(), Some(blog::actions::posts::show::path(&post.slug).as_str()));
    assert_eq!(app.db.unread_notifications(&ada).await?, 1);
    app.assert_mailed("ada@example.com", "Grace commented").await?;
    Ok(())
}

#[kiln::test]
async fn the_inbox_page_marks_notifications_read(app: TestApp) -> Result {
    let (ada, post) = comment_on_ada_post(&app).await?;
    app.perform_enqueued_jobs().await?;
    let client = app.sign_in_as(&ada);
    client.get_html(notifications::index::path()).await.assert_status(200).assert_contains("(1 unread)");
    let id = app.db.inbox(&ada, 1).await?[0].id;
    client
        .patch_form(notifications::read::path(id), &[])
        .await
        .assert_redirect(&blog::actions::posts::show::path(&post.slug));
    assert_eq!(app.db.unread_notifications(&ada).await?, 0);
    client
        .patch_form(notifications::read_all::path(), &[])
        .await
        .assert_flash("Marked 0 read.")
        .assert_redirect(&notifications::index::path());
    Ok(())
}

#[kiln::test]
async fn mark_all_read_goes_back_to_the_page_it_was_on(app: TestApp) -> Result {
    let ada = app.create::<User>().await?;
    let client = || app.sign_in_as(&ada).header("host", "blog.test");
    let from_post = client().header("referer", "https://blog.test/posts/engines?page=2#comments");
    from_post.patch_form(notifications::read_all::path(), &[]).await.assert_redirect("/posts/engines?page=2");
    // Another site's page is never a destination.
    let elsewhere = client().header("referer", "https://evil.example/posts/engines");
    elsewhere.patch_form(notifications::read_all::path(), &[]).await.assert_redirect(&notifications::index::path());
    Ok(())
}

#[kiln::test]
async fn the_nav_counts_unread_notifications_from_layout_data(app: TestApp) -> Result {
    let (ada, post) = comment_on_ada_post(&app).await?;
    app.perform_enqueued_jobs().await?;
    let client = app.sign_in_as(&ada);
    let page = blog::actions::posts::show::path(&post.slug);
    client.get_html(&page).await.assert_status(200).assert_contains("id=\"nav-notifications\">Notifications (1)</a>");
    client.patch_form(notifications::read_all::path(), &[]).await.assert_flash("Marked 1 read.");
    client.get_html(&page).await.assert_contains("id=\"nav-notifications\">Notifications</a>");
    // Signed out, the hook gets no user (and the nav has no link).
    app.client().get_html(&page).await.assert_status(200).assert_not_contains("nav-notifications");
    // A Turbo Frame request renders no layout (so its `layout_data` isn't computed).
    let frame = app.sign_in_as(&ada).header("Turbo-Frame", "comments").get_html(&page).await;
    frame.assert_status(200).assert_not_contains("nav-notifications");
    Ok(())
}

#[kiln::test]
async fn the_inbox_says_how_long_ago_by_the_test_clock(app: TestApp) -> Result {
    let (ada, _) = comment_on_ada_post(&app).await?;
    app.perform_enqueued_jobs().await?;
    let client = app.sign_in_as(&ada);
    client.get_html(notifications::index::path()).await.assert_contains(">just now</time>");
    // Rounded down: 2h29m is still "2 hours".
    app.travel(SignedDuration::from_mins(149));
    client.get_html(notifications::index::path()).await.assert_contains(">2 hours ago</time>");
    app.travel(SignedDuration::from_hours(24 * 4));
    client.get_html(notifications::index::path()).await.assert_contains(">4 days ago</time>");
    Ok(())
}

#[kiln::test]
async fn nobody_reads_someone_elses_notifications(app: TestApp) -> Result {
    let (ada, _) = comment_on_ada_post(&app).await?;
    app.perform_enqueued_jobs().await?;
    let id = app.db.inbox(&ada, 1).await?[0].id;
    let stranger = app.create::<User>().await?;
    assert!(!app.db.mark_notification_read(&stranger, id).await?);
    app.sign_in_as(&stranger).patch_form(notifications::read::path(id), &[]).await.assert_status(303);
    assert_eq!(app.db.unread_notifications(&ada).await?, 1, "still unread");
    Ok(())
}

#[kiln::test]
async fn deleted_recipients_get_nothing(app: TestApp) -> Result {
    let comment = app.create::<Comment>().await?;
    let gone = app.create::<User>().await?;
    CommentPosted { comment_id: comment.id }.notify(&app.db, &gone).await?;
    User::delete(gone.id).run(&app.db).await?;
    app.perform_enqueued_jobs().await?;
    assert!(app.outbox().await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn authors_can_turn_comment_emails_off(app: TestApp) -> Result {
    use kiln::notify::Channel;
    let ada = app.create_with::<User>(|u| u.email = "ada@example.com".into()).await?;
    app.db.set_notification_pref(&ada, "CommentPosted", Channel::Mail, false).await?;
    let post = app.create_with::<Post>(|p| p.author_id = ada.id).await?;
    let grace = app.create::<User>().await?;
    app.sign_in_as(&grace).post_form(comments::create::path(&post.slug), &[("body", "Hi")]).await.assert_status(303);
    app.perform_enqueued_jobs().await?;
    assert_eq!(app.db.inbox(&ada, 10).await?.len(), 1, "still in the inbox");
    assert!(app.outbox().await?.is_empty(), "no email");
    Ok(())
}

#[kiln::test]
async fn every_email_off_and_inbox_off_both_hold(app: TestApp) -> Result {
    use kiln::notify::Channel;
    let ada = app.create::<User>().await?;
    app.db.set_notification_pref(&ada, "*", Channel::Mail, false).await?;
    app.db.set_notification_pref(&ada, "CommentPosted", Channel::Inbox, false).await?;
    let comment = app.create::<Comment>().await?;
    CommentPosted { comment_id: comment.id }.notify(&app.db, &ada).await?;
    app.perform_enqueued_jobs().await?;
    assert!(app.db.inbox(&ada, 10).await?.is_empty());
    assert!(app.outbox().await?.is_empty());

    let prefs = app.db.notification_prefs(&ada).await?;
    assert!(prefs.iter().any(|p| p.kind == "*" && p.channel == Channel::Mail && !p.enabled));
    assert!(prefs.iter().any(|p| p.kind == "CommentPosted" && p.description == "Someone comments on your post"));
    assert!(
        app.db.set_notification_pref(&ada, "CommentPosted", Channel::Broadcast, false).await.is_err(),
        "not optional"
    );
    Ok(())
}
