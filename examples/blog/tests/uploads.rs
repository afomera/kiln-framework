//! File uploads: a post's cover image, from the form to the page and back.

use blog::{actions::posts, records::*};
use kiln::{
    prelude::*,
    testing::{TestApp, Upload},
    time::SignedDuration,
};

/// The `value` of the hidden input that keeps a form's current file.
fn kept_file_token(html: &str, field: &str) -> String {
    let marker = format!("type=\"hidden\" name=\"{field}\" value=\"");
    let start = html.find(&marker).map(|i| i + marker.len()).expect("the form keeps the current file");
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

fn img_src(html: &str) -> String {
    let start = html.find("class=\"cover\" src=\"").map(|i| i + "class=\"cover\" src=\"".len()).expect("cover shown");
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

#[kiln::test]
async fn an_author_uploads_a_cover_and_readers_see_it(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client.get_html(posts::new::path()).await.assert_contains(r#"enctype="multipart/form-data""#).assert_contains(
        r#"<input type="file" id="cover" name="cover" accept="image/*" data-direct-upload-url="/kiln/direct_uploads">"#,
    );
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "With a cover"), ("body", "<p>Look.</p>"), ("publish", "true")],
            &[("cover", Upload::png("sunset.png"))],
        )
        .await
        .assert_redirect(&posts::show::path("with-a-cover"));

    let post = Post::find_by(post::slug.eq("with-a-cover")).one_or_404(&app.db).await?;
    let cover = post.cover.clone().expect("the cover was saved");
    assert_eq!((cover.filename(), cover.content_type()), ("sunset.png", "image/png"));
    assert_eq!(app.ctx().storage()?.read(&cover).await?.len(), Upload::png("x").bytes.len());

    // Pages show a resized copy; links can't be forged.
    let page = app.get_html(posts::show::path("with-a-cover")).await;
    let src = img_src(&page.text);
    assert!(src.starts_with("/kiln/variants/") && src.ends_with("/sunset.png"), "{src}");
    app.get(&src).await.assert_status(200);
    app.get(src.replacen("/kiln/variants/", "/kiln/variants/x", 1)).await.assert_status(404);

    // Anyone with the original's link gets it, shown inline (the edit form links it).
    let edit = client.get_html(posts::edit::path("with-a-cover")).await;
    let marker = "class=\"field-file\"><img src=\"";
    let start = edit.text.find(marker).expect("the edit form shows the cover") + marker.len();
    let original = edit.text[start..start + edit.text[start..].find('"').unwrap()].to_owned();
    assert!(original.starts_with("/kiln/files/"), "{original}");
    let image = app.get(&original).await;
    image.assert_status(200);
    assert_eq!(image.headers["content-type"], "image/png");
    assert_eq!(image.headers["x-content-type-options"], "nosniff");
    assert!(image.headers["content-disposition"].to_str().unwrap().starts_with("inline"));
    app.get(original.replacen("/kiln/files/", "/kiln/files/x", 1)).await.assert_status(404);
    Ok(())
}

#[kiln::test]
async fn covers_must_be_images_and_the_form_keeps_a_good_one(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "Not an image"), ("body", "<p>x</p>")],
            &[("cover", Upload::text("notes.txt", "hello"))],
        )
        .await
        .assert_status(422)
        .assert_contains("Cover must be image/*");
    // Named and labelled as a PNG, but it isn't one: the bytes decide.
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "Fake image"), ("body", "<p>x</p>")],
            &[("cover", Upload::new("fake.png", "image/png", b"nope".to_vec()))],
        )
        .await
        .assert_status(422)
        .assert_contains("Cover must be image/*");

    // A valid file survives a re-render for another field's error: no re-upload needed.
    let page = client
        .post_multipart(
            posts::create::path(),
            &[("title", "Hi"), ("body", "<p>x</p>")],
            &[("cover", Upload::png("a.png"))],
        )
        .await;
    page.assert_status(422).assert_contains("Title is too short").assert_contains("a.png");
    let token = kept_file_token(&page.text, "cover");
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "Fixed title"), ("body", "<p>x</p>"), ("cover", &token)],
            &[],
        )
        .await
        .assert_redirect(&posts::show::path("fixed-title"));
    let post = Post::find_by(post::slug.eq("fixed-title")).one_or_404(&app.db).await?;
    assert_eq!(post.cover.as_ref().map(|c| c.filename()), Some("a.png"));
    Ok(())
}

#[kiln::test]
async fn editing_keeps_the_cover_unless_a_new_one_is_chosen(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "Edited later"), ("body", "<p>x</p>")],
            &[("cover", Upload::png("first.png"))],
        )
        .await
        .assert_status(303);
    let edit = client.get_html(posts::edit::path("edited-later")).await;
    edit.assert_contains("first.png");
    let token = kept_file_token(&edit.text, "cover");

    // An empty file input (the browser sends no filename) keeps the current file.
    client
        .patch_multipart(
            posts::update::path("edited-later"),
            &[("title", "Edited later"), ("body", "<p>y</p>"), ("cover", &token)],
            &[("cover", Upload::new("", "application/octet-stream", Vec::new()))],
        )
        .await
        .assert_status(303);
    let post = Post::find_by(post::slug.eq("edited-later")).one_or_404(&app.db).await?;
    assert_eq!(post.cover.as_ref().map(|c| c.filename()), Some("first.png"));

    client
        .patch_multipart(
            posts::update::path("edited-later"),
            &[("title", "Edited later"), ("body", "<p>y</p>"), ("cover", &token)],
            &[("cover", Upload::png("second.png"))],
        )
        .await
        .assert_status(303);
    let post = Post::find_by(post::slug.eq("edited-later")).one_or_404(&app.db).await?;
    assert_eq!(post.cover.as_ref().map(|c| c.filename()), Some("second.png"));
    Ok(())
}

#[kiln::test]
async fn forms_cannot_claim_files_they_did_not_upload(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .post_form(posts::create::path(), &[("title", "Sneaky"), ("body", "<p>x</p>"), ("cover", r#"{"blob":1}"#)])
        .await
        .assert_status(400);
    Ok(())
}

#[kiln::test]
async fn unattached_files_are_purged_and_attached_ones_kept(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let kept = app.ctx().attach("kept.png", "image/png", Upload::png("kept.png").bytes).await?;
    let stray = app.ctx().attach("stray.png", "image/png", Upload::png("stray.png").bytes).await?;
    app.create_with::<Post>(|p| {
        p.author_id = author.id;
        p.cover = Some(kept.clone());
    })
    .await?;
    assert_eq!(kiln::storage::purge_unattached(app.ctx(), SignedDuration::from_hours(48)).await?, 0, "too new");
    app.travel(SignedDuration::from_hours(72));
    assert_eq!(kiln::storage::purge_unattached(app.ctx(), SignedDuration::from_hours(48)).await?, 1);
    assert!(app.ctx().storage()?.read(&kept).await.is_ok());
    assert!(app.ctx().storage()?.read(&stray).await.is_err());
    Ok(())
}

/// The `value` of a hidden input named `name`.
fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html.find(&marker).map(|i| i + marker.len()).expect("hidden input present");
    html[start..start + html[start..].find('"').unwrap()].to_owned()
}

#[kiln::test]
async fn database_errors_from_pending_migrations_offer_to_run_them(app: TestApp) -> Result {
    // The app's migrations are SQLite's; Postgres test runs build the schema from the records.
    if app.db.dialect().name() != "sqlite" {
        return Ok(());
    }
    let post = app.create::<Post>().await?;
    // As if the cover migration hadn't run yet.
    app.db.execute_script("ALTER TABLE \"posts\" DROP COLUMN \"cover\"").await?;
    app.db.execute_script("DELETE FROM \"kiln_migrations\" WHERE \"version\" = '20261013040730'").await?;

    let page = app.get_html(posts::show::path(&post.slug)).await;
    page.assert_status(500)
        .assert_contains("20261013040730_add_post_covers")
        .assert_contains(r#"action="/kiln/resolve/migrate""#)
        .assert_contains("Run migrations");
    let token = hidden(&page.text, "_kiln");
    let back = posts::show::path(&post.slug);
    app.client().post_form("/kiln/resolve/migrate", &[("_kiln", &token), ("back", &back)]).await.assert_status(303);
    app.get_html(back).await.assert_status(200).assert_contains(&post.title);
    Ok(())
}

#[kiln::test]
async fn the_editor_uploads_attachments_that_posts_show(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .get_html(posts::new::path())
        .await
        .assert_contains(r#"data-direct-upload-url="/kiln/direct_uploads""#)
        .assert_contains(r#""@rails/activestorage":"/kiln/direct-upload-"#);

    // What Lexxy's upload does: the details (with the page's CSRF token), then the bytes.
    let blob = client.direct_upload(Upload::png("sun.png")).await;
    assert_eq!((blob["filename"].as_str(), blob["content_type"].as_str()), (Some("sun.png"), Some("image/png")));
    let url = blob["url"].as_str().expect("url").to_owned();

    // What the editor then submits.
    let body = format!(
        r#"<p>Look:</p><kiln-attachment sgid="{sgid}" url="{url}" content-type="image/png" filename="sun.png" filesize="70" width="1" height="1" caption="The sun" presentation="gallery"></kiln-attachment><kiln-attachment url="/kiln/files/forged.x/evil.png" content-type="image/png"></kiln-attachment>"#,
        sgid = blob["attachable_sgid"].as_str().unwrap()
    );
    client
        .post_form(posts::create::path(), &[("title", "Sunny"), ("body", &body), ("publish", "true")])
        .await
        .assert_status(303);
    let page = app.get_html(posts::show::path("sunny")).await;
    page.assert_contains(r#"<figure class="attachment attachment--preview attachment--png">"#)
        .assert_contains(&format!(r#"<img src="{url}""#))
        .assert_contains("The sun")
        .assert_not_contains("evil.png");
    app.get(&url).await.assert_status(200);

    // Files that rich text links are kept.
    app.travel(SignedDuration::from_hours(72));
    assert_eq!(kiln::storage::purge_unattached(app.ctx(), SignedDuration::from_hours(48)).await?, 0);
    app.get(&url).await.assert_status(200);
    Ok(())
}

#[kiln::test]
async fn uploads_need_the_pages_csrf_token(app: TestApp) -> Result {
    // A client with no page loaded has no session token to send.
    let response = app
        .client()
        .post_json("/kiln/direct_uploads", &kiln::testing::json!({ "blob": { "filename": "x.png", "byte_size": 1 } }))
        .await;
    response.assert_status(403);
    // And the bytes only go where a signed link says.
    app.client()
        .request(kiln::testing::Method::PUT, "/kiln/direct_uploads/forged.token", None)
        .await
        .assert_status(403);
    Ok(())
}

#[kiln::test]
async fn forms_take_direct_uploads_by_token(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .get_html(posts::new::path())
        .await
        .assert_contains(r#"accept="image/*" data-direct-upload-url="/kiln/direct_uploads""#);
    // What the form's JavaScript does on submit: upload first, then send only the token.
    let blob = client.direct_upload(Upload::png_sized("direct.png", 40, 20)).await;
    let token = blob["upload_token"].as_str().expect("a form token").to_owned();
    client
        .post_form(posts::create::path(), &[("title", "Direct"), ("body", "<p>x</p>"), ("cover", &token)])
        .await
        .assert_status(303);
    let post = Post::find_by(post::slug.eq("direct")).one_or_404(&app.db).await?;
    let cover = post.cover.expect("the uploaded cover");
    assert_eq!((cover.filename(), cover.content_type()), ("direct.png", "image/png"));
    assert_eq!(app.ctx().storage()?.read(&cover).await?.len(), Upload::png_sized("x", 40, 20).bytes.len());
    Ok(())
}

#[kiln::test]
async fn direct_uploads_respect_the_size_limit(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    let token = client.csrf_token();
    // Declared too big: refused before any bytes move. (The default cap is 25MB.)
    let response = client
        .post_json_with_csrf(
            "/kiln/direct_uploads",
            &kiln::testing::json!({ "blob": { "filename": "huge.bin", "byte_size": 100_000_000_u64 } }),
            &token,
        )
        .await;
    response.assert_status(413);
    Ok(())
}

/// Width and height from a PNG's header.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[kiln::test]
async fn covers_are_resized_once_and_never_enlarged(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    let client = app.sign_in_as(&author);
    client
        .post_multipart(
            posts::create::path(),
            &[("title", "Big cover"), ("body", "<p>x</p>"), ("publish", "true")],
            &[("cover", Upload::png_sized("big.png", 2400, 1200))],
        )
        .await
        .assert_status(303);
    let src = img_src(&app.get_html(posts::show::path("big-cover")).await.text);
    let resized = app.get(&src).await;
    resized.assert_status(200);
    assert_eq!(resized.headers["content-type"], "image/png");
    let body = resized.bytes.clone();
    assert_eq!(png_size(&body), (1200, 600), "fits the 1200px width, keeping the ratio");
    // Served from storage the second time: same bytes.
    assert_eq!(app.get(&src).await.bytes, body);
    Ok(())
}
