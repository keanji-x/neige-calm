//! #2131 S4: `POST /api/track-recipes` under an `Idempotency-Key`. A retry under the key is
//! answered with the recipe its first attempt saved; the same key with another body is 409
//! `idempotency_key_reused`; a malformed key is 400 `idempotency_key_invalid`; and a duplicate that
//! passes the dedup read is joined by the `operations` UNIQUE backstop.

use std::time::Duration;

use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
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

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored recipe.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_recipe() {
    let boot = boot().await;
    let paused = PausePoint {
        entered: std::sync::Arc::new(Notify::new()),
        release: std::sync::Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "k-recipe-race", paused.clone());
    let (first, second) = tokio::join!(
        create(boot.app.clone(), recipe("mine"), Some("k-recipe-race")),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
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
