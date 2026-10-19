//! Inserting: optional fields by default, back-dated timestamps and many rows at once.
//! Runs on Postgres too with `KILN_TEST_DATABASE_URL`.

use blog::records::*;
use kiln::{prelude::*, testing::TestApp, time::SignedDuration};

fn new_bookmark(owner: &User, title: &str) -> NewBookmark {
    NewBookmark::new(BookmarkRequired {
        title: title.into(),
        url: format!("https://{title}.example"),
        owner_id: owner.id,
    })
}

#[kiln::test]
async fn new_records_default_their_optional_fields(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    let plain = Bookmark::insert(new_bookmark(&owner, "plain")).run(&app.db).await?;
    // `notes`/`rating` are `Option`s, `pinned` is `#[default]`.
    assert_eq!((plain.notes, plain.rating, plain.pinned), (None, None, false));
    let rated = Bookmark::insert(NewBookmark { rating: Some(4), ..new_bookmark(&owner, "rated") }).run(&app.db).await?;
    assert_eq!(rated.rating, Some(4));
    Ok(())
}

#[kiln::test]
async fn inserts_take_their_own_timestamps(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    let before = Timestamp::now();
    let now = Bookmark::insert(new_bookmark(&owner, "now")).run(&app.db).await?;
    assert!(now.created_at >= before && now.updated_at == now.created_at);

    // `updated_at` follows a back-dated `created_at`, unless it's set too.
    let then = Timestamp::days_ago(30);
    let old = Bookmark::insert(new_bookmark(&owner, "old")).created_at(then).run(&app.db).await?;
    assert_eq!((old.created_at, old.updated_at), (then, then));
    let edited = Timestamp::days_ago(2);
    let row = Bookmark::insert(new_bookmark(&owner, "edited")).updated_at(edited).created_at(then).run(&app.db).await?;
    assert_eq!((row.created_at, row.updated_at), (then, edited));
    assert_eq!(Bookmark::find(row.id).one_or_404(&app.db).await?.created_at, then);
    Ok(())
}

#[kiln::test]
async fn insert_all_writes_many_rows_in_order(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    assert!(Bookmark::insert_all(Vec::<NewBookmark>::new()).run(&app.db).await?.is_empty());

    // Each row may carry its own `created_at`.
    let start = Timestamp::days_ago(10);
    let rows = (0..3).map(|n| {
        let at = start + SignedDuration::from_hours(n);
        Bookmark::insert(new_bookmark(&owner, &format!("b{n}"))).created_at(at)
    });
    let queries = app.db.query_count();
    let stored = Bookmark::insert_all(rows).run(&app.db).await?;
    assert_eq!(app.db.query_count() - queries, 1, "one statement");
    let titles: Vec<&str> = stored.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["b0", "b1", "b2"]);
    assert_eq!(stored[2].created_at, start + SignedDuration::from_hours(2));

    // Thousands of rows go in chunks, all in one transaction, still in order.
    let many: Vec<NewBookmark> = (0..2500).map(|n| new_bookmark(&owner, &format!("m{n:04}"))).collect();
    let stored = Bookmark::insert_all(many).created_at(start).run(&app.db).await?;
    assert_eq!(stored.len(), 2500);
    assert!(stored.windows(2).all(|w| w[0].title < w[1].title && w[0].id < w[1].id));
    assert!(stored.iter().all(|b| b.created_at == start));
    assert_eq!(Bookmark::query().filter(bookmark::title.like("m%")).count(&app.db).await?, 2500);
    Ok(())
}

#[kiln::test]
async fn insert_all_rolls_back_every_chunk_on_failure(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    // The last row breaks the users' unique email index, in the third chunk.
    let mut users: Vec<NewUser> = (0..2001)
        .map(|n| {
            NewUser::new(UserRequired {
                name: format!("U{n}"),
                email: format!("u{n}@example.com"),
                password_digest: String::new(),
            })
        })
        .collect();
    users.push(NewUser::new(UserRequired {
        name: "Dup".into(),
        email: owner.email.clone(),
        password_digest: String::new(),
    }));
    assert!(User::insert_all(users).run(&app.db).await.is_err());
    assert_eq!(User::query().count(&app.db).await?, 1);

    // Inside a transaction, they're part of it.
    let ids = app
        .db
        .transaction(|tx| async move {
            let rows = vec![new_bookmark(&owner, "t1"), new_bookmark(&owner, "t2")];
            Ok(Bookmark::insert_all(rows).run(&tx).await?.into_iter().map(|b| b.id).collect::<Vec<_>>())
        })
        .await?;
    assert_eq!(Bookmark::query().filter(bookmark::id.is_in(&ids)).count(&app.db).await?, 2);
    Ok(())
}
