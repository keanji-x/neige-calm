//! #2131 S4: the keyed answers of `POST /api/tracks/{id}/cards`, on both of its branches. A
//! retry under one `Idempotency-Key` is answered with the first card and calls no plugin tool
//! again; the same key with another body is 409 `idempotency_key_reused`; a malformed key is 400
//! `idempotency_key_invalid` before anything runs; and a duplicate that passes the dedup read is
//! joined by the `operations` UNIQUE backstop.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
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

/// The first request is held inside the operation insert, after its tool call; a retry under the
/// same key meanwhile waits on the key's claim instead of calling the tool a second time, and is
/// then answered with the first request's card.
#[tokio::test]
async fn a_concurrent_retry_waits_for_the_first_tool_call_instead_of_making_its_own() {
    let fx = Arc::new(card_fixture("test.toolcall.keyed-race").await);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "k-tool-race", paused.clone());
    let first = tokio::spawn({
        let fx = fx.clone();
        async move { post_keyed(&fx, via(&fx, json!({})), Some("k-tool-race")).await }
    });
    tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
        .await
        .expect("the first request reached its insert; without it the case is vacuous");
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

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored card.
#[tokio::test]
async fn a_direct_duplicate_past_the_dedup_check_joins_the_stored_card() {
    let fx = Arc::new(card_fixture("test.toolcall.direct-race").await);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "k-direct-race", paused.clone());
    let (first, second) = tokio::join!(
        post_keyed(&fx, direct("Notes"), Some("k-direct-race")),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
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
        "one operation: the first request's INSERT was refused by the index"
    );
    fx.state.plugin.stop(&fx.plugin_id).await.ok();
}
