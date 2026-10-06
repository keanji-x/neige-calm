//! #2131 S4: `POST /api/track-recipes` under an `Idempotency-Key`. A retry under the key is
//! answered with the recipe its first attempt saved; the same key with another body is 409
//! `idempotency_key_reused`; a malformed key is 400 `idempotency_key_invalid`; and a concurrent
//! duplicate is joined to the stored recipe by the key check inside its commit transaction.

use std::time::Duration;

use calm_server::test_seams::{OPERATION_KEYED_COMMIT_BEGIN, PausePoint, install_pause_for_test};
use tokio::sync::Notify;

use super::*;

async fn create(app: axum::Router, body: Value, key: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/track-recipes")
        .header("X-Calm-Actor", "user")
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn recipes(app: axum::Router) -> usize {
    let (status, list) = send(app, "GET", "/api/track-recipes", None, None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list.as_array().expect("recipe list").len()
}

fn recipe(title: &str) -> Value {
    json!({ "title": title, "body": "# Plan\n\nWrite the change.\n" })
}

#[tokio::test]
async fn a_create_replayed_under_its_key_is_the_first_recipe() {
    let boot = boot().await;
    let (status, first) = create(boot.app.clone(), recipe("mine"), Some("k-recipe")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, replay) = create(boot.app.clone(), recipe("mine"), Some("k-recipe")).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(
        replay, first,
        "the replay answers the stored recipe, byte for byte"
    );
    assert_eq!(recipes(boot.app.clone()).await, 1);
    // Without a key every request is its own create.
    let (status, unkeyed) = create(boot.app.clone(), recipe("mine"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{unkeyed}");
    assert_ne!(unkeyed["id"], first["id"]);
    assert_eq!(recipes(boot.app.clone()).await, 2);
}

#[tokio::test]
async fn the_same_key_with_another_body_is_reused_and_saves_nothing() {
    let boot = boot().await;
    let (status, first) = create(boot.app.clone(), recipe("mine"), Some("k-recipe")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, answer) = create(boot.app.clone(), recipe("other"), Some("k-recipe")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_reused", "{answer}");
    assert_eq!(recipes(boot.app.clone()).await, 1);
}

#[tokio::test]
async fn an_over_long_or_blank_key_is_invalid_and_saves_nothing() {
    let boot = boot().await;
    for key in ["k".repeat(129), "   ".to_string()] {
        let (status, answer) = create(boot.app.clone(), recipe("mine"), Some(&key)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(answer["code"], "idempotency_key_invalid", "{answer}");
    }
    assert_eq!(recipes(boot.app.clone()).await, 0);
}

/// The first request is paused just before its commit transaction; the second commits the operation
/// under the same key meanwhile. The first's transaction then finds the key bound to the same
/// request and joins the stored recipe instead of saving a second one.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_recipe() {
    let boot = boot().await;
    let paused = PausePoint {
        entered: std::sync::Arc::new(Notify::new()),
        release: std::sync::Arc::new(Notify::new()),
    };
    install_pause_for_test(
        OPERATION_KEYED_COMMIT_BEGIN,
        "k-recipe-race",
        paused.clone(),
    );
    let (first, second) = tokio::join!(
        create(boot.app.clone(), recipe("mine"), Some("k-recipe-race")),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request reached its commit; without it the case is vacuous");
            let second = create(boot.app.clone(), recipe("mine"), Some("k-recipe-race")).await;
            paused.release.notify_one();
            second
        }
    );
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(recipes(boot.app.clone()).await, 1);
}

/// #2175 K1a: a recipe save commits in its own transaction and never queues behind the global
/// drive lock, which track delete holds (and a Codex thread start holds for up to 30 s).
#[tokio::test]
async fn a_keyed_create_does_not_wait_for_the_track_delete_lock() {
    let boot = boot().await;
    let _guard = boot
        .state
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    let (status, created) = tokio::time::timeout(
        Duration::from_secs(5),
        create(boot.app.clone(), recipe("mine"), Some("k-recipe-unlocked")),
    )
    .await
    .expect("the save must not wait for the drive lock");
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(recipes(boot.app.clone()).await, 1);
}

/// #2175 K1a: a recipe save drives no one else's operation. A claimable pending row of another
/// kind keeps its phase and stays unleased across the save.
#[tokio::test]
async fn a_create_leaves_another_kinds_pending_operation_unclaimed() {
    use calm_server::operation::card_create_adapter::CardCreateOperationPayload;
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let boot = boot().await;
    let pool = boot.state.raw_repo().sqlite_pool().expect("sqlite repo");
    let other = SqlxOperationRepo::new(pool.clone())
        .insert_operation(
            "card-create",
            OperationKey {
                operation_key: "seeded-other-kind".into(),
                idempotency_key: None,
                payload_hash: "seeded".into(),
            },
            serde_json::to_value(CardCreateOperationPayload {
                actor: calm_server::ids::ActorId::User,
                correlation: None,
                track_id: "no-such-track".into(),
                kind: "note".into(),
                sort: None,
                payload: Value::Null,
                title: None,
            })
            .unwrap(),
        )
        .await
        .expect("seed a pending operation");
    let (status, created) =
        create(boot.app.clone(), recipe("mine"), Some("k-recipe-no-drive")).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT phase, lease_owner FROM operations WHERE id = ?1")
            .bind(&other)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        ("pending".to_string(), None),
        "the recipe save must not claim or drive another operation"
    );
}
