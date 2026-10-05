//! #2068: the keyed-write answers of `POST /api/tracks/{id}/terminal-cards` — an over-long key,
//! the same key with a different body, and a duplicate that passes the operation dedup check and
//! reaches the `operations` UNIQUE backstop.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{boot_happy, post_with_idempotency};

fn body(program: &str) -> Value {
    json!({ "program": program, "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} })
}

async fn operations_under(boot: &super::Boot, key: &str) -> i64 {
    let pool = boot.repo.sqlite_pool().expect("sqlite repo");
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1")
        .bind(key)
        .fetch_one(&pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_over_long_key_is_invalid_and_submits_nothing() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let key = "k".repeat(129);
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri, body("/bin/sh"), Some(&key)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_invalid", "{answer}");
    assert_eq!(operations_under(&boot, &key).await, 0);
}

#[tokio::test]
async fn the_same_key_with_another_body_is_reused_and_final() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri.clone(), body("/bin/sh"), Some("k-1")).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri, body("/bin/bash"), Some("k-1")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_reused", "{answer}");
    assert_eq!(operations_under(&boot, "k-1").await, 1);
}

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored operation.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_card() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "k-race", paused.clone());
    let (first, second) = tokio::join!(
        post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            body("/bin/sh"),
            Some("k-race")
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
            let second = post_with_idempotency(
                boot.app.clone(),
                uri.clone(),
                body("/bin/sh"),
                Some("k-race"),
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
        operations_under(&boot, "k-race").await,
        1,
        "one operation: the first request's INSERT was refused by the index"
    );
}
