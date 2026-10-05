//! #2068: the keyed-write answers of `POST /api/tracks/{id}/claude-cards` beyond the ones its
//! parent file pins (a non-ASCII key, the same key with a different body): an over-long key, and a
//! duplicate that passes the operation dedup check and reaches the `operations` UNIQUE backstop.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
use tokio::sync::Notify;

use super::{Boot, ENV_LOCK, body, boot_success, post};

async fn operations_under(boot: &Boot, key: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1")
        .bind(key)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_over_long_key_is_invalid_and_submits_nothing() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let key = "k".repeat(129);
    let (status, answer) = post(
        boot.app.clone(),
        &boot.track_id,
        body(None),
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_invalid", "{answer}");
    assert_eq!(operations_under(&boot, &key).await, 0);
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 0);
}

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored operation: one card,
/// one spawn.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_card() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot_success().await;
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "claude-race", paused.clone());
    let (first, second) = tokio::join!(
        post(
            boot.app.clone(),
            &boot.track_id,
            body(None),
            Some("claude-race"),
            None
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
            let second = post(
                boot.app.clone(),
                &boot.track_id,
                body(None),
                Some("claude-race"),
                None,
            )
            .await;
            paused.release.notify_one();
            second
        }
    );
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(
        operations_under(&boot, "claude-race").await,
        1,
        "one operation: the first request's INSERT was refused by the index"
    );
    assert_eq!(boot.spawn_count.load(Ordering::SeqCst), 1);
}
