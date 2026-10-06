//! #2131 S4: the keyed answers of `POST /api/tracks/{id}/cards`, on both of its branches. A
//! retry under one `Idempotency-Key` is answered with the first card and calls no plugin tool
//! again; the same key with another body is 409 `idempotency_key_reused`; a malformed key is 400
//! `idempotency_key_invalid` before anything runs; and a duplicate that misses the route's replay
//! read is joined to the stored card by the key check inside its commit transaction.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::test_seams::{OPERATION_KEYED_COMMIT_BEGIN, PausePoint, install_pause_for_test};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use super::{Fixture, StubConfig, app, body_to_json, boot};

async fn card_fixture(plugin_id: &str) -> Fixture {
    boot(StubConfig {
        plugin_id,
        mode: "card",
        cards_create: true,
    })
    .await
}

fn via(fx: &Fixture, arguments: Value) -> Value {
    json!({
        "via_tool_call": {
            "plugin_id": fx.plugin_id,
            "tool_name": "make_status_card",
            "arguments": arguments,
        }
    })
}

fn direct(title: &str) -> Value {
    json!({ "kind": "ui://stub/status", "payload": { "msg": "hi" }, "title": title })
}

async fn post_keyed(fx: &Fixture, body: Value, key: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/api/tracks/{}/cards", fx.track_id))
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = app(fx.state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (status, body_to_json(response).await)
}

async fn cards(fx: &Fixture) -> usize {
    fx.state
        .repo
        .cards_by_track(&fx.track_id)
        .await
        .expect("list cards")
        .len()
}

async fn operations_under(fx: &Fixture, key: &str) -> i64 {
    let pool = fx.state.raw_repo().sqlite_pool().expect("sqlite repo");
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1")
        .bind(key)
        .fetch_one(&pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_tool_call_replayed_under_its_key_is_the_first_card_and_calls_the_tool_once() {
    let fx = card_fixture("test.toolcall.keyed-replay").await;
    let (status, first) = post_keyed(&fx, via(&fx, json!({ "n": 1 })), Some("k-tool")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, replay) = post_keyed(&fx, via(&fx, json!({ "n": 1 })), Some("k-tool")).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(
        fx.tool_calls(),
        1,
        "the replay must not call the plugin tool again"
    );
    assert_eq!(cards(&fx).await, 1);
    // Without a key every request is its own create: the second one calls the tool again.
    let (status, unkeyed) = post_keyed(&fx, via(&fx, json!({ "n": 1 })), None).await;
    assert_eq!(status, StatusCode::CREATED, "{unkeyed}");
    assert_ne!(unkeyed["id"], first["id"]);
    assert_eq!(fx.tool_calls(), 2);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn the_same_key_with_another_tool_call_is_reused_and_calls_nothing() {
    let fx = card_fixture("test.toolcall.keyed-reused").await;
    let (status, first) = post_keyed(&fx, via(&fx, json!({ "n": 1 })), Some("k-tool")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, answer) = post_keyed(&fx, via(&fx, json!({ "n": 2 })), Some("k-tool")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_reused", "{answer}");
    assert_eq!(fx.tool_calls(), 1);
    assert_eq!(cards(&fx).await, 1);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn an_over_long_key_is_invalid_before_the_tool_is_called() {
    let fx = card_fixture("test.toolcall.keyed-invalid").await;
    let key = "k".repeat(129);
    for body in [via(&fx, json!({})), direct("Notes")] {
        let (status, answer) = post_keyed(&fx, body, Some(&key)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(answer["code"], "idempotency_key_invalid", "{answer}");
    }
    assert_eq!(fx.tool_calls(), 0);
    assert_eq!(cards(&fx).await, 0);
    assert_eq!(operations_under(&fx, &key).await, 0);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

/// The first request is held just before its commit transaction, after its tool call; a retry under
/// the same key meanwhile waits on the key's in-process lock instead of calling the tool a second
/// time, and is then answered with the first request's card.
#[tokio::test]
async fn a_concurrent_retry_waits_for_the_first_tool_call_instead_of_making_its_own() {
    let fx = Arc::new(card_fixture("test.toolcall.keyed-race").await);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_KEYED_COMMIT_BEGIN, "k-tool-race", paused.clone());
    let first = tokio::spawn({
        let fx = fx.clone();
        async move { post_keyed(&fx, via(&fx, json!({})), Some("k-tool-race")).await }
    });
    tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
        .await
        .expect("the first request reached its commit; without it the case is vacuous");
    assert_eq!(fx.tool_calls(), 1);
    let second = tokio::spawn({
        let fx = fx.clone();
        async move { post_keyed(&fx, via(&fx, json!({})), Some("k-tool-race")).await }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !second.is_finished(),
        "the retry must wait for the request holding its key"
    );
    paused.release.notify_one();
    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(fx.tool_calls(), 1);
    assert_eq!(operations_under(&fx, "k-tool-race").await, 1);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

#[tokio::test]
async fn a_direct_create_replayed_under_its_key_is_the_first_card_and_another_body_is_reused() {
    let fx = card_fixture("test.toolcall.direct-keyed").await;
    let (status, first) = post_keyed(&fx, direct("Notes"), Some("k-direct")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, replay) = post_keyed(&fx, direct("Notes"), Some("k-direct")).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    let (status, answer) = post_keyed(&fx, direct("Other"), Some("k-direct")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_reused", "{answer}");
    assert_eq!(cards(&fx).await, 1);
    assert_eq!(operations_under(&fx, "k-direct").await, 1);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

/// The first request is paused just before its commit transaction; the second commits the operation
/// under the same key meanwhile. The first's transaction then finds the key bound to the same
/// request and joins the stored card instead of writing a second one.
#[tokio::test]
async fn a_direct_duplicate_past_the_dedup_check_joins_the_stored_card() {
    let fx = Arc::new(card_fixture("test.toolcall.direct-race").await);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        OPERATION_KEYED_COMMIT_BEGIN,
        "k-direct-race",
        paused.clone(),
    );
    let (first, second) = tokio::join!(
        post_keyed(&fx, direct("Notes"), Some("k-direct-race")),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request reached its commit; without it the case is vacuous");
            let second = post_keyed(&fx, direct("Notes"), Some("k-direct-race")).await;
            paused.release.notify_one();
            second
        }
    );
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(cards(&fx).await, 1);
    assert_eq!(
        operations_under(&fx, "k-direct-race").await,
        1,
        "one operation: the first request's transaction wrote nothing"
    );
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

async fn operation_row(fx: &Fixture, id: &str) -> (String, Option<String>) {
    let pool = fx.state.raw_repo().sqlite_pool().expect("sqlite repo");
    sqlx::query_as("SELECT phase, lease_owner FROM operations WHERE id = ?1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
}

/// #2175 K1a: a card create commits in its own transaction and never queues behind the global
/// drive lock, which track delete holds (and a Codex thread start holds for up to 30 s).
#[tokio::test]
async fn a_keyed_create_does_not_wait_for_the_track_delete_lock() {
    let fx = card_fixture("test.toolcall.keyed-unlocked").await;
    let _guard = fx
        .state
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    for (body, key) in [
        (direct("Notes"), "k-unlocked-direct"),
        (via(&fx, json!({})), "k-unlocked-tool"),
    ] {
        let (status, card) =
            tokio::time::timeout(Duration::from_secs(5), post_keyed(&fx, body, Some(key)))
                .await
                .expect("the create must not wait for the drive lock");
        assert_eq!(status, StatusCode::CREATED, "{card}");
        assert_eq!(operations_under(&fx, key).await, 1);
    }
    assert_eq!(cards(&fx).await, 2);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

/// #2175 K1a: a card create drives no one else's operation. A claimable pending row of another
/// kind keeps its phase and stays unleased across the create.
#[tokio::test]
async fn a_create_leaves_another_kinds_pending_operation_unclaimed() {
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let fx = card_fixture("test.toolcall.keyed-no-drive").await;
    let pool = fx.state.raw_repo().sqlite_pool().expect("sqlite repo");
    let other = SqlxOperationRepo::new(pool)
        .insert_operation(
            "track-recipe-create",
            OperationKey {
                operation_key: "seeded-other-kind".into(),
                idempotency_key: None,
                payload_hash: "seeded".into(),
            },
            json!({ "title": "seeded", "body": "# Plan\n" }),
        )
        .await
        .expect("seed a pending operation");
    let (status, card) = post_keyed(&fx, direct("Notes"), Some("k-no-drive")).await;
    assert_eq!(status, StatusCode::CREATED, "{card}");
    assert_eq!(
        operation_row(&fx, &other).await,
        ("pending".to_string(), None),
        "the card create must not claim or drive another operation"
    );
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

/// #2175 K1a: a `card-create` row an older server left past its commit (`tx_committed`) is still
/// driven to its end, and a retry under its key on either branch is answered with its card.
#[tokio::test]
async fn a_legacy_committed_row_replays_its_card() {
    let fx = card_fixture("test.toolcall.keyed-legacy").await;
    let pool = fx.state.raw_repo().sqlite_pool().expect("sqlite repo");
    for (body, key) in [
        (direct("Notes"), "k-legacy-direct"),
        (via(&fx, json!({})), "k-legacy-tool"),
    ] {
        let (status, first) = post_keyed(&fx, body.clone(), Some(key)).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        sqlx::query("UPDATE operations SET phase = 'tx_committed' WHERE idempotency_key = ?1")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
        let (status, replay) = post_keyed(&fx, body, Some(key)).await;
        assert_eq!(status, StatusCode::CREATED, "{replay}");
        assert_eq!(replay["id"], first["id"]);
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM operations WHERE idempotency_key = ?1")
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            phase, "succeeded",
            "the replay drove the legacy row to its end"
        );
    }
    assert_eq!(cards(&fx).await, 2);
    assert_eq!(fx.tool_calls(), 1);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}

/// #2175 K1a: a create refused inside its transaction stores nothing, so its key binds nothing and a
/// corrected retry under the same key is a fresh create.
#[tokio::test]
async fn a_refused_create_binds_nothing_to_its_key() {
    let fx = card_fixture("test.toolcall.keyed-refused").await;
    // A client may not mint a track-report card; the card write refuses it inside the transaction.
    let refused = json!({
        "kind": "track-report",
        "payload": calm_server::track_report::TrackReportPayload::initial(),
    });
    let (status, answer) = post_keyed(&fx, refused, Some("k-refused")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(operations_under(&fx, "k-refused").await, 0);
    assert_eq!(cards(&fx).await, 0);
    let (status, card) = post_keyed(&fx, direct("Notes"), Some("k-refused")).await;
    assert_eq!(status, StatusCode::CREATED, "{card}");
    assert_eq!(cards(&fx).await, 1);
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}
