//! The app's own template helpers: `| avatar_hue` (src/helpers/) and the `avatar(..)` macro
//! (templates/helpers/), available in every template without an import.

use blog::{actions::posts, helpers::avatar_hue, records::*};
use kiln::{prelude::*, testing::TestApp};

#[test]
fn avatar_hues_are_stable_and_in_range() {
    assert_eq!(avatar_hue("Grace"), avatar_hue("Grace"));
    assert_ne!(avatar_hue("Grace"), avatar_hue("Ada"));
    assert!(["", "Grace", "Ada Lovelace", "李"].iter().all(|n| avatar_hue(n) < 360));
}

#[kiln::test]
async fn comments_show_an_avatar_from_the_global_macro(app: TestApp) -> Result {
    let post = app.create::<Post>().await?;
    let grace = app.create_with::<User>(|u| u.name = "grace".into()).await?;
    app.create_with::<Comment>(|c| {
        c.post_id = post.id;
        c.author_id = grace.id;
    })
    .await?;
    let hue = avatar_hue("grace");
    app.client().get_html(posts::show::path(&post.slug)).await.assert_status(200).assert_contains(&format!(
        r#"<span class="avatar" style="--avatar-hue: {hue}" aria-hidden="true">G</span> <strong>grace</strong>"#
    ));
    Ok(())
}
