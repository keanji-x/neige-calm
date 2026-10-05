//! #2068: the keyed-write answers of `POST /api/tracks/{id}/conversations` — a malformed or
//! over-long key, the same key with a different body, and a duplicate that passes the operation
//! dedup check and reaches the `operations` UNIQUE backstop.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::conversation_keys::derive_track_conversation_operation_key_for_test;
use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
use tokio::sync::Notify;

use super::boot;

#[tokio::test]
async fn an_over_long_key_is_invalid_and_mints_nothing() {
    let b = boot().await;
    let track_id = b.create_track("keyed-too-long").await;
    let (status, body) = b
        .create_conversation(&track_id, &"k".repeat(129), "hello")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "idempotency_key_invalid", "{body}");
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'",
            &track_id
        )
        .await,
        0
    );
}

#[tokio::test]
async fn the_same_key_with_other_text_is_reused_and_final() {
    let b = boot().await;
    let track_id = b.create_track("keyed-reused").await;
    let (status, body) = b.create_conversation(&track_id, "k-1", "first").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = b.create_conversation(&track_id, "k-1", "second").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "idempotency_key_reused", "{body}");
    assert_eq!(b.copies_in_harness("second", 1).await, 0);
    b.shutdown_harnesses().await;
}

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored operation: both are
/// answered with the one conversation, and the message is delivered once.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_conversation() {
    let b = boot().await;
    let track_id = b.create_track("keyed-backstop").await;
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let operation_key = derive_track_conversation_operation_key_for_test(&track_id, "k-race");
    install_pause_for_test(OPERATION_DEDUP_MISSED, &operation_key, paused.clone());
    let (first, second) = tokio::join!(
        b.create_conversation(&track_id, "k-race", "only once"),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
            let second = b
                .create_conversation(&track_id, "k-race", "only once")
                .await;
            paused.release.notify_one();
            second
        }
    );
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(
        b.scalar(
            "SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1",
            &operation_key
        )
        .await,
        1,
        "one operation: the first request's INSERT was refused by the index"
    );
    assert_eq!(b.copies_in_harness("only once", 1).await, 1);
    b.shutdown_harnesses().await;
}
