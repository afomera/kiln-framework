//! The query layer past plain filters: subqueries, aggregates, row locks, atomic
//! increments and raw SQL. Runs on Postgres too with `KILN_TEST_DATABASE_URL`.

use blog::records::*;
use kiln::{prelude::*, testing::TestApp};

fn titles(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|p| p.title.as_str()).collect()
}

#[kiln::test]
async fn subqueries_filter_through_other_tables(app: TestApp) -> Result {
    let ada = app.create_with::<User>(|u| u.name = "Ada".into()).await?;
    let bob = app.create_with::<User>(|u| u.name = "Bob".into()).await?;
    let mine = app.create_with::<Post>(|p| (p.title, p.author_id) = ("Mine".into(), ada.id)).await?;
    let commented = app.create_with::<Post>(|p| (p.title, p.author_id) = ("Commented".into(), bob.id)).await?;
    let edited = app
        .create_with::<Post>(|p| (p.title, p.author_id, p.editor_id) = ("Edited".into(), bob.id, Some(ada.id)))
        .await?;
    app.create_with::<Post>(|p| (p.title, p.author_id) = ("Other".into(), bob.id)).await?;
    app.create_with::<Comment>(|c| (c.post_id, c.author_id) = (commented.id, ada.id)).await?;

    // A `Query<User>` yields its ids, for a foreign key.
    let by_ada = Post::query().filter(post::author_id.in_query(User::query().filter(user::name.eq("Ada"))));
    assert_eq!(titles(&by_ada.all(&app.db).await?), ["Mine"]);

    // An `Option<Id<User>>` column takes the same subquery.
    let edited_by_ada = Post::query().filter(post::editor_id.in_query(User::query().filter(user::name.eq("Ada"))));
    assert_eq!(titles(&edited_by_ada.all(&app.db).await?), ["Edited"]);

    // A union of subqueries: what an access policy looks like.
    let visible = Post::query()
        .filter(
            post::author_id
                .eq(ada.id)
                .or(post::id.in_query(Comment::query().filter(comment::author_id.eq(ada.id)).select(comment::post_id)))
                .or(post::editor_id.eq(ada.id)),
        )
        .order(post::id.asc());
    let ids: Vec<Id<Post>> = visible.all(&app.db).await?.iter().map(|p| p.id).collect();
    assert_eq!(ids, [mine.id, commented.id, edited.id]);

    // NOT IN skips the NULLs a nullable subquery column yields; they'd otherwise match nothing.
    let never_edited_anyone = User::query().filter(user::id.not_in_query(Post::query().select(post::editor_id)));
    let names: Vec<String> = never_edited_anyone.all(&app.db).await?.into_iter().map(|u| u.name).collect();
    assert!(names.contains(&"Bob".to_owned()) && !names.contains(&"Ada".to_owned()), "{names:?}");

    // `select` plucks a column.
    let slugs = Post::query().filter(post::author_id.eq(ada.id)).select(post::slug).all(&app.db).await?;
    assert_eq!(slugs, [mine.slug]);
    Ok(())
}

#[kiln::test]
async fn subqueries_keep_their_limits_and_binds_in_order(app: TestApp) -> Result {
    let author = app.create::<User>().await?;
    for n in 1..=3 {
        app.create_with::<Post>(|p| (p.title, p.author_id) = (format!("P{n}"), author.id)).await?;
    }
    let newest_two = Post::query().filter(post::author_id.eq(author.id)).order(post::id.desc()).limit(2);
    let posts = Post::query()
        .filter(post::title.ne("P3"))
        .filter(post::id.in_query(newest_two))
        .filter(post::title.like("P%"))
        .all(&app.db)
        .await?;
    assert_eq!(titles(&posts), ["P2"]);
    Ok(())
}

#[kiln::test]
async fn aggregates_count_by_and_sum(app: TestApp) -> Result {
    let ada = app.create::<User>().await?;
    for (rating, pinned) in [(Some(5), true), (Some(3), false), (None, false), (Some(4), true)] {
        app.create_with::<Bookmark>(|b| (b.owner_id, b.rating, b.pinned) = (ada.id, rating, pinned)).await?;
    }
    let other = app.create_with::<Bookmark>(|b| (b.rating, b.title) = (Some(1), "Zebra".into())).await?;
    let mine = Bookmark::query().filter(bookmark::owner_id.eq(ada.id));

    assert_eq!(mine.clone().count_by(bookmark::pinned, &app.db).await?, [(false, 2), (true, 2)]);
    assert_eq!(Bookmark::query().count_by(bookmark::owner_id, &app.db).await?, [(ada.id, 4), (other.owner_id, 1)]);
    assert_eq!(
        mine.clone().count_by(bookmark::rating, &app.db).await?,
        [(None, 1), (Some(3), 1), (Some(4), 1), (Some(5), 1)]
    );
    assert_eq!(mine.clone().sum(bookmark::rating, &app.db).await?, 12);
    assert_eq!(mine.clone().max(bookmark::rating, &app.db).await?, Some(5));
    assert_eq!(mine.min(bookmark::rating, &app.db).await?, Some(3));
    assert_eq!(Bookmark::query().max(bookmark::title, &app.db).await?.as_deref(), Some("Zebra"));

    // No rows: sums are 0, extremes are None.
    let none = Bookmark::query().filter(bookmark::owner_id.eq(Id::new(0)));
    assert_eq!(none.clone().sum(bookmark::rating, &app.db).await?, 0);
    assert_eq!(none.clone().max(bookmark::created_at, &app.db).await?, None);
    assert!(none.count_by(bookmark::pinned, &app.db).await?.is_empty());
    Ok(())
}

#[kiln::test]
async fn increments_are_atomic_and_return_the_new_value(app: TestApp) -> Result {
    let bookmark = app.create::<Bookmark>().await?;
    assert_eq!(bookmark.rating, None);
    // NULL counts as 0.
    let one = Bookmark::update(bookmark.id).increment(bookmark::rating, 1).run(&app.db).await?;
    assert_eq!(one.rating, Some(1));
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let db = app.db.clone();
            tokio::spawn(async move { Bookmark::update(bookmark.id).increment(bookmark::rating, 2).run(&db).await })
        })
        .collect();
    for handle in handles {
        handle.await.expect("task")?;
    }
    let back =
        Bookmark::update(bookmark.id).increment(bookmark::rating, -1).set(bookmark::pinned, true).run(&app.db).await?;
    assert_eq!((back.rating, back.pinned), (Some(10), true));
    Ok(())
}

#[kiln::test]
async fn for_update_locks_inside_a_transaction(app: TestApp) -> Result {
    let bookmark = app.create::<Bookmark>().await?;
    let rating = app
        .db
        .transaction(|tx| async move {
            let locked = Bookmark::find(bookmark.id).for_update().one_or_404(&tx).await?;
            let next = locked.rating.unwrap_or(0) + 1;
            Bookmark::update(bookmark.id).set(bookmark::rating, Some(next)).run(&tx).await?;
            Ok(next)
        })
        .await?;
    assert_eq!(rating, 1);

    // Outside a transaction the lock would end with the query: refused, with what to do.
    let err = Bookmark::find(bookmark.id).for_update().one(&app.db).await.unwrap_err();
    assert!(err.to_string().contains("inside `db.transaction"), "{err}");
    Ok(())
}

#[kiln::test]
async fn raw_sql_is_the_escape_hatch(app: TestApp) -> Result {
    let ada = app.create_with::<User>(|u| u.name = "Ada".into()).await?;
    let first = app.create_with::<Post>(|p| (p.title, p.author_id) = ("First?".into(), ada.id)).await?;
    app.create_with::<Post>(|p| p.title = "Elsewhere".into()).await?;

    // A join ordered by the other table, mapped to records.
    let posts: Vec<Post> = app
        .db
        .sql(
            "SELECT \"posts\".* FROM \"posts\" JOIN \"users\" ON \"users\".\"id\" = \"posts\".\"author_id\" \
             WHERE \"users\".\"name\" = ? AND \"posts\".\"title\" <> '?' ORDER BY \"users\".\"name\", \"posts\".\"id\"",
        )
        .bind("Ada")
        .records()
        .await?;
    assert_eq!(titles(&posts), ["First?"]);

    // Tuples and single values, decoded by position.
    let pairs: Vec<(Id<Post>, String)> =
        app.db.sql("SELECT \"id\", \"title\" FROM \"posts\" WHERE \"author_id\" = ?").bind(ada.id).all().await?;
    assert_eq!(pairs, [(first.id, "First?".to_owned())]);
    let count: Option<i64> = app.db.sql("SELECT COUNT(*) FROM \"posts\"").one().await?;
    assert_eq!(count, Some(2));

    // JSON rows, keyed by column name.
    let rows = app
        .db
        .sql("SELECT \"title\", \"editor_id\", CAST(1.5 AS DOUBLE PRECISION) AS \"score\" FROM \"posts\" WHERE \"id\" = ?")
        .bind(first.id)
        .json()
        .await?;
    assert_eq!(
        serde_json::Value::Object(rows[0].clone()),
        serde_json::json!({"title": "First?", "editor_id": null, "score": 1.5})
    );

    // Writes give the rows affected, and run inside a transaction like any query.
    let before = app.db.query_count();
    let changed = app
        .db
        .transaction(|tx| async move {
            tx.sql("UPDATE \"posts\" SET \"title\" = ? WHERE \"author_id\" = ?")
                .bind("Renamed")
                .bind(ada.id)
                .run()
                .await
        })
        .await?;
    assert_eq!(changed, 1);
    assert!(app.db.query_count() > before, "raw SQL is counted with the other queries");
    assert_eq!(Post::find(first.id).one_or_404(&app.db).await?.title, "Renamed");

    let err = app.db.sql("SELECT \"id\" FROM \"posts\"").all::<(i64, String)>().await.unwrap_err();
    assert!(err.to_string().contains("select one column per tuple element"), "{err}");
    Ok(())
}
