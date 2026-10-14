//! `for_update` under real concurrency: transactions that read a counter and write it back
//! must queue, not lose increments. On a SQLite file (the in-memory test database locks
//! per table instead), or on Postgres when `KILN_TEST_DATABASE_URL` names one.

use std::time::Duration;

use kiln::prelude::*;

use counter::{Counter, NewCounter};

mod counter {
    use kiln::prelude::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "kiln_test_lock_counters")]
    pub struct Counter {
        pub id: Id<Counter>,
        pub value: i64,
    }
}

async fn database() -> Result<(Db, Option<std::path::PathBuf>)> {
    if let Ok(url) = std::env::var("KILN_TEST_DATABASE_URL") {
        return Ok((Db::connect_or_create(&url).await?, None));
    }
    let path = std::env::temp_dir().join(format!("kiln-row-locks-{}.sqlite3", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Ok((Db::connect(&format!("sqlite://{}", path.display())).await?, Some(path)))
}

#[tokio::test]
async fn for_update_serializes_read_then_write() -> Result {
    let (db, file) = database().await?;
    db.execute("DROP TABLE IF EXISTS \"kiln_test_lock_counters\"").await?;
    db.execute(
        "CREATE TABLE \"kiln_test_lock_counters\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"value\" INTEGER NOT NULL)",
    )
    .await?;
    let counter = Counter::insert(NewCounter { value: 0 }).run(&db).await?;

    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                db.transaction(|tx| async move {
                    let current = Counter::find(counter.id).for_update().one_or_404(&tx).await?;
                    // Long enough that every other task reads in the meantime, were it allowed to.
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Counter::update(counter.id).set(counter::value, current.value + 1).run(&tx).await
                })
                .await
            })
        })
        .collect();
    for task in tasks {
        task.await.expect("task")?;
    }
    let value = Counter::find(counter.id).one_or_404(&db).await?.value;
    db.execute("DROP TABLE \"kiln_test_lock_counters\"").await?;
    if let Some(file) = file {
        let _ = std::fs::remove_file(file);
    }
    assert_eq!(value, 5);
    Ok(())
}
