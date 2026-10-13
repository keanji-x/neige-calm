//! #2596 — `/api/read-receipts` through the production router: the user's read state lives in the
//! kernel. A mark read keeps the larger instant and clears a manual unread, a mark unread sets it,
//! both are idempotent and the user's alone, and the migration's baseline answers every read.
use super::auth::{app, dev_auth_state, fresh_state};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::Actor;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn list(app: &axum::Router) -> Value {
    let (status, body) = send(
        app,
        Request::builder()
            .uri("/api/read-receipts")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn mark_read(
    app: &axum::Router,
    kind: &str,
    id: &str,
    at: i64,
    actor: &str,
) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/read-receipts/{kind}/{id}/read"))
            .header("content-type", "application/json")
            .header(Actor::HEADER, actor)
            .body(Body::from(
                serde_json::to_vec(&json!({ "readThroughMs": at })).unwrap(),
            ))
            .unwrap(),
    )
    .await
}

async fn mark_unread(app: &axum::Router, kind: &str, id: &str, actor: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/read-receipts/{kind}/{id}/unread"))
            .header(Actor::HEADER, actor)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

fn receipt(kind: &str, id: &str, at: i64, manual: bool) -> Value {
    json!({ "kind": kind, "targetId": id, "readThroughMs": at, "manualUnread": manual })
}

#[tokio::test]
async fn mark_read_keeps_the_larger_instant() {
    let app = app(fresh_state().await, dev_auth_state());
    assert_eq!(
        mark_read(&app, "track", "t1", 200, "user").await,
        (StatusCode::OK, receipt("track", "t1", 200, false))
    );
    assert_eq!(
        mark_read(&app, "track", "t1", 100, "user").await,
        (StatusCode::OK, receipt("track", "t1", 200, false)),
        "an older instant never lowers the receipt"
    );
    assert_eq!(
        mark_read(&app, "track", "t1", 300, "user").await,
        (StatusCode::OK, receipt("track", "t1", 300, false))
    );
    assert_eq!(
        list(&app).await["receipts"],
        json!([receipt("track", "t1", 300, false)])
    );
}

#[tokio::test]
async fn writes_are_idempotent_and_mark_read_clears_a_manual_unread() {
    let app = app(fresh_state().await, dev_auth_state());
    for _ in 0..2 {
        assert_eq!(
            mark_read(&app, "conversation", "c1", 50, "user").await,
            (StatusCode::OK, receipt("conversation", "c1", 50, false))
        );
    }
    for _ in 0..2 {
        assert_eq!(
            mark_unread(&app, "conversation", "c1", "user").await,
            (StatusCode::OK, receipt("conversation", "c1", 50, true)),
            "a manual unread keeps the instant read through"
        );
    }
    // A target never read is unread by hand at zero.
    assert_eq!(
        mark_unread(&app, "track", "never", "user").await,
        (StatusCode::OK, receipt("track", "never", 0, true))
    );
    assert_eq!(
        mark_read(&app, "conversation", "c1", 10, "user").await,
        (StatusCode::OK, receipt("conversation", "c1", 50, false)),
        "a mark read clears the manual unread even when its instant is older"
    );
    // The same id under the other kind is another receipt.
    assert_eq!(
        list(&app).await["receipts"],
        json!([
            receipt("conversation", "c1", 50, false),
            receipt("track", "never", 0, true)
        ])
    );
}

#[tokio::test]
async fn an_unknown_kind_or_a_negative_instant_is_refused() {
    let app = app(fresh_state().await, dev_auth_state());
    for (status, body) in [
        mark_read(&app, "area", "a1", 1, "user").await,
        mark_unread(&app, "area", "a1", "user").await,
        mark_read(&app, "track", "t1", -1, "user").await,
    ] {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].is_string(), "{body}");
    }
    assert_eq!(list(&app).await["receipts"], json!([]));
}

#[tokio::test]
async fn only_the_user_writes_receipts() {
    let app = app(fresh_state().await, dev_auth_state());
    for actor in ["ai:codex", "ai:planner"] {
        let (status, body) = mark_read(&app, "track", "t1", 1, actor).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {body}");
        let (status, body) = mark_unread(&app, "track", "t1", actor).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {body}");
    }
    assert_eq!(list(&app).await["receipts"], json!([]));
}

#[tokio::test]
async fn the_migration_writes_one_baseline_that_writes_never_move() {
    let before = calm_server::model::now_ms();
    let state = fresh_state().await;
    let after = calm_server::model::now_ms();
    let app = app(state, dev_auth_state());
    let first = list(&app).await;
    let baseline = first["baselineMs"].as_i64().expect("baselineMs");
    // SQLite's `julianday('now')` keeps milliseconds; allow its rounding at the edges.
    assert!(
        (before - 1..=after + 1).contains(&baseline),
        "baseline {baseline} outside the migration window [{before}, {after}]"
    );
    assert_eq!(first["receipts"], json!([]));
    mark_read(&app, "track", "t1", baseline + 1_000, "user").await;
    mark_unread(&app, "track", "t2", "user").await;
    assert_eq!(list(&app).await["baselineMs"], json!(baseline));
}
