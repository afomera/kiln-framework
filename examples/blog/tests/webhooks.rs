//! Outbound webhooks: captured in tests, signed, retried, logged and redelivered.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use blog::records::*;
use kiln::{
    prelude::*,
    testing::{TestApp, json},
    time::SignedDuration,
    webhooks::{self, Status, Webhook},
};

const SECRET: &str = "whsec_test";

#[kiln::test]
async fn deliveries_are_captured_signed_and_sent_only_after_commit(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    let hook = Webhook::new("https://hooks.example.com/kiln", SECRET).for_record(&owner);
    let delivery_id = app
        .db
        .transaction(|tx| {
            let hook = hook.clone();
            async move { hook.deliver_later(&tx, "post.published", &json!({ "title": "Engines" })).await }
        })
        .await?;
    assert!(app.webhooks_sent().await?.is_empty(), "delivery is a job");
    app.assert_enqueued::<webhooks::DeliverWebhook>(1).await?;

    app.perform_enqueued_jobs().await?;
    let sent = app.assert_webhook_sent("post.published").await?;
    assert_eq!(sent.status, Status::Captured);
    assert_eq!(sent.delivery_id, delivery_id);
    assert_eq!(sent.json()["title"], "Engines");
    assert_eq!(sent.header("x-kiln-event"), Some("post.published"));
    assert_eq!(sent.header("X-Kiln-Delivery"), Some(delivery_id.as_str()));
    let signature = sent.header("X-Kiln-Signature").expect("signed");
    webhooks::verify(SECRET, signature, &sent.request_body, SignedDuration::from_mins(5))?;
    assert!(webhooks::verify("wrong", signature, &sent.request_body, SignedDuration::from_mins(5)).is_err());
    assert!(!format!("{:?}", sent.request_headers).contains(SECRET), "the secret is never logged");
    assert_eq!(app.db.webhook_deliveries(&owner, 10).await?.len(), 1);
    Ok(())
}

#[kiln::test]
async fn a_rolled_back_write_sends_nothing(app: TestApp) -> Result {
    let result: Result = app
        .db
        .transaction(|tx| async move {
            Webhook::new("https://hooks.example.com", SECRET).deliver_later(&tx, "push", &json!({})).await?;
            Err(Error::Conflict("nope".into()))
        })
        .await;
    assert!(result.is_err());
    app.assert_enqueued::<webhooks::DeliverWebhook>(0).await?;
    Ok(())
}

#[kiln::test]
async fn failures_retry_and_every_attempt_is_logged(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    app.receive_webhooks(move |request| {
        // A receiver verifies the signature like this.
        let signature = request.header("x-kiln-signature").unwrap_or("");
        if webhooks::verify(SECRET, signature, &request.body, SignedDuration::from_mins(5)).is_err() {
            return (400, "bad signature".into());
        }
        match seen.fetch_add(1, Ordering::SeqCst) {
            0 => (500, "boom".into()),
            1 => (503, "x".repeat(10_000)),
            _ => (202, "thanks".into()),
        }
    });
    Webhook::new("https://ci.example.com/hook", SECRET)
        .for_record(&owner)
        .deliver_later(&app.db, "push", &json!({ "ref": "main" }))
        .await?;
    app.perform_enqueued_jobs().await?;

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let log = app.db.webhook_deliveries(&owner, 10).await?;
    let summary: Vec<_> = log.iter().rev().map(|d| (d.attempt, d.status, d.response_status)).collect();
    assert_eq!(
        summary,
        [(1, Status::Failed, Some(500)), (2, Status::Failed, Some(503)), (3, Status::Succeeded, Some(202))]
    );
    assert_eq!(log[2].error.as_deref(), Some("HTTP 500"));
    assert_eq!(log[1].response_body.as_ref().map(String::len), Some(webhooks::RESPONSE_EXCERPT), "bounded");
    assert_eq!(log[0].response_body.as_deref(), Some("thanks"));
    assert!(log.iter().all(|d| d.delivery_id == log[0].delivery_id), "one delivery id across retries");
    Ok(())
}

#[kiln::test]
async fn a_receiver_that_never_succeeds_fails_the_job_for_good(app: TestApp) -> Result {
    app.receive_webhooks(|_| (500, String::new()));
    Webhook::new("https://down.example.com", SECRET).deliver_later(&app.db, "push", &json!({})).await?;
    let err = app.perform_enqueued_jobs().await.unwrap_err();
    assert!(err.to_string().contains("DeliverWebhook"), "{err}");
    assert!(err.to_string().contains("down.example.com: HTTP 500"), "{err}");
    assert_eq!(app.webhooks_sent().await?.len(), 15, "every attempt is logged");
    Ok(())
}

#[kiln::test]
async fn redelivery_reuses_the_delivery_and_stays_with_its_owner(app: TestApp) -> Result {
    let owner = app.create::<User>().await?;
    let stranger = app.create::<User>().await?;
    let hook = Webhook::new("https://old.example.com", SECRET).for_record(&owner);
    hook.deliver_later(&app.db, "issue.opened", &json!({ "number": 7 })).await?;
    app.perform_enqueued_jobs().await?;
    let first = app.assert_webhook_sent("issue.opened").await?;

    let moved = Webhook::new("https://new.example.com", "rotated").for_record(&owner);
    let theirs = Webhook::new("https://evil.example.com", "x").for_record(&stranger);
    assert!(matches!(theirs.redeliver(&app.db, first.id).await, Err(Error::NotFound)));
    moved.redeliver(&app.db, first.id).await?;
    app.perform_enqueued_jobs().await?;

    let again = app.db.webhook_delivery_attempts(&first.delivery_id).await?;
    assert_eq!(again.len(), 2);
    let redelivered = &again[1];
    assert!(redelivered.redelivery);
    assert_eq!(redelivered.attempt, 2);
    assert_eq!(redelivered.url, "https://new.example.com");
    assert_eq!(redelivered.request_body, first.request_body);
    let signature = redelivered.header("x-kiln-signature").unwrap();
    webhooks::verify("rotated", signature, &redelivered.request_body, SignedDuration::from_mins(5))?;
    Ok(())
}

#[tokio::test]
async fn gone_stops_retrying_and_tells_the_app() -> Result {
    let migrations = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/db/migrations"));
    let gone = Arc::new(AtomicUsize::new(0));
    let told = gone.clone();
    let app = TestApp::configured(migrations, |app| {
        Ok(blog::app::configure(app)?.provide(
            webhooks::Options::new().header_prefix("X-Blog").hub_signature().on_gone(move |_ctx, delivery| {
                let told = told.clone();
                async move {
                    assert_eq!(delivery.status, Status::Gone);
                    assert!(delivery.record_id.is_some());
                    told.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        ))
    })
    .await?;
    let owner = app.create::<User>().await?;
    app.receive_webhooks(|request| {
        assert!(request.header("X-Blog-Signature").is_some(), "the prefix is configurable");
        let hub = request.header("X-Hub-Signature-256").unwrap_or("");
        webhooks::verify_hub_signature(SECRET, hub, &request.body).expect("GitHub-style signature");
        (410, "unsubscribed".into())
    });
    Webhook::new("https://gone.example.com", SECRET)
        .for_record(&owner)
        .deliver_later(&app.db, "push", &json!({}))
        .await?;
    app.perform_enqueued_jobs().await?;
    let log = app.webhooks_sent().await?;
    assert_eq!(log.len(), 1, "410 isn't retried");
    assert_eq!(log[0].status, Status::Gone);
    assert_eq!(gone.load(Ordering::SeqCst), 1);
    Ok(())
}

#[kiln::test]
async fn deliver_now_returns_the_attempt_and_the_log_page_shows_it(app: TestApp) -> Result {
    app.receive_webhooks(|_| (200, "pong".into()));
    let ping =
        Webhook::new("https://hooks.example.com/ping", SECRET).deliver_now(app.ctx(), "ping", &json!({})).await?;
    assert!(ping.succeeded());
    assert_eq!(ping.response_body.as_deref(), Some("pong"));
    app.get("/kiln/webhooks").await.assert_status(200).assert_contains("ping").assert_contains("hooks.example.com");
    app.get(format!("/kiln/webhooks/{}", ping.id)).await.assert_status(200).assert_contains("X-Kiln-Signature");
    Ok(())
}
