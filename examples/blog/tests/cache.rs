//! The cache: fetch, expiry, counters, and `/kiln/cache`.

use std::sync::atomic::{AtomicUsize, Ordering};

use kiln::{prelude::*, testing::TestApp, time::SignedDuration};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Stats {
    posts: i64,
}

#[kiln::test]
async fn fetch_computes_once_until_it_expires(app: TestApp) -> Result {
    let cache = app.ctx().cache();
    let runs = AtomicUsize::new(0);
    let compute = || async {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(Stats { posts: 3 })
    };
    let ttl = SignedDuration::from_mins(5);
    assert_eq!(cache.fetch("stats", ttl, compute).await?, Stats { posts: 3 });
    assert_eq!(cache.fetch("stats", ttl, compute).await?, Stats { posts: 3 });
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the second fetch is a hit");

    app.travel(SignedDuration::from_mins(6));
    assert_eq!(cache.get::<Stats>("stats").await?, None, "expired");
    cache.fetch("stats", ttl, compute).await?;
    assert_eq!(runs.load(Ordering::SeqCst), 2);

    // Workers sweep what's expired.
    app.travel(SignedDuration::from_mins(12));
    assert_eq!(kiln::cache::purge_expired(&app.db).await?, 1);
    Ok(())
}

#[kiln::test]
async fn set_get_delete_and_prefixes(app: TestApp) -> Result {
    let cache = app.ctx().cache();
    let ttl = SignedDuration::from_hours(1);
    cache.set("post:1:title", "Hello", ttl).await?;
    cache.set("post:1:words", &250, ttl).await?;
    cache.set("post:10:title", "Other", ttl).await?;
    assert_eq!(cache.get::<String>("post:1:title").await?.as_deref(), Some("Hello"));
    // A value that no longer has the expected shape is a miss, not an error.
    assert_eq!(cache.get::<Stats>("post:1:title").await?, None);

    assert!(cache.delete("post:1:title").await?);
    assert!(!cache.delete("post:1:title").await?);
    // `_` and `%` in a prefix are literal, so `post:1:` doesn't match `post:10:`.
    assert_eq!(cache.delete_prefix("post:1:").await?, 1);
    assert_eq!(cache.get::<String>("post:10:title").await?.as_deref(), Some("Other"));
    assert!(cache.set("", &1, ttl).await.is_err(), "empty keys are refused");
    assert_eq!(cache.clear().await?, 1);
    Ok(())
}

#[kiln::test]
async fn counters_count_and_restart_after_expiry(app: TestApp) -> Result {
    let cache = app.ctx().cache();
    let hour = SignedDuration::from_hours(1);
    assert_eq!(cache.increment("hits", 1, hour).await?, 1);
    assert_eq!(cache.increment("hits", 1, hour).await?, 2);
    assert_eq!(cache.increment("hits", 5, hour).await?, 7);
    // Counting doesn't extend the window: an hour after the first hit it starts over.
    app.travel(SignedDuration::from_mins(61));
    assert_eq!(cache.increment("hits", 1, hour).await?, 1);
    Ok(())
}

#[kiln::test]
async fn concurrent_increments_each_get_their_own_number(app: TestApp) -> Result {
    let cache = app.ctx().cache();
    let hour = SignedDuration::from_hours(1);
    let counts = futures_join(&cache, hour).await?;
    let mut sorted = counts.clone();
    sorted.sort();
    assert_eq!(sorted, (1..=8).collect::<Vec<i64>>(), "{counts:?}");
    Ok(())
}

async fn futures_join(cache: &kiln::cache::Cache, ttl: SignedDuration) -> Result<Vec<i64>> {
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let cache = cache.clone();
            tokio::spawn(async move { cache.increment("busy", 1, ttl).await })
        })
        .collect();
    let mut counts = vec![];
    for task in tasks {
        counts.push(task.await.expect("the task ran")?);
    }
    Ok(counts)
}

#[kiln::test]
async fn the_command_center_lists_and_clears_the_cache(app: TestApp) -> Result {
    app.ctx().cache().set("dashboard:stats", &Stats { posts: 9 }, SignedDuration::from_mins(5)).await?;
    let client = app.client();
    let page = client.get_html("/kiln/cache").await;
    page.assert_status(200).assert_contains("dashboard:stats").assert_contains("1 live entry");
    let start = page.text.find("name=_kiln value=\"").expect("a control form") + "name=_kiln value=\"".len();
    let token = page.text[start..start + page.text[start..].find('"').unwrap()].to_owned();
    client
        .post_form_without_csrf("/kiln/cache/clear", &[("_kiln", &token), ("back", "/kiln/cache")])
        .await
        .assert_status(303);
    assert_eq!(app.ctx().cache().get::<Stats>("dashboard:stats").await?, None);
    Ok(())
}
