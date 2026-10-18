//! #2068: an over-long `Idempotency-Key` on `POST /api/tracks` is refused by the shared parser on
//! both create shapes, before anything is minted. The other keyed answers (a different create under
//! a used key, the cross-instance primary-key race) are pinned in the parent file.

use axum::http::StatusCode;

use super::boot;

#[tokio::test]
async fn an_over_long_key_is_invalid_on_both_create_shapes_and_mints_nothing() {
    let b = boot().await;
    let key = "k".repeat(129);
    for message in [Some("a first message"), None] {
        let (status, body) = b.create_track(Some(&key), message).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "message={message:?} body={body}"
        );
        assert_eq!(
            body["code"], "idempotency_key_invalid",
            "message={message:?} body={body}"
        );
    }
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.binding_count().await, 0);
}

#[tokio::test]
async fn create_admin_refuses_ai_actor_headers() {
    use super::*;
    let b = boot_without_daemon().await;
    let body = json!({"planner_provider":"codex","area_id":b.area_id,"admin":true,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
    for actor in ["ai:codex", "ai:claude", "ai:planner", "ai:custom"] {
        let response = b
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tracks")
                    .header("content-type", "application/json")
                    .header("x-calm-actor", actor)
                    .header("idempotency-key", "admin-key")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{actor}");
    }
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.card_count().await, 0);
    let pool = b.repo.sqlite_db().unwrap();
    for table in ["events", "track_create_idempotency"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let (status, created) = b.post_create(Some("admin-key"), body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(created["admin_granted_at"].is_i64());
    let before = b.track_count().await;
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .header("x-calm-actor", "ai:claude")
                .header("idempotency-key", "admin-key")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(b.track_count().await, before);
}

#[tokio::test]
async fn create_key_reused_with_other_admin_conflicts() {
    use super::*;
    let b = boot_without_daemon().await;
    let mut body = json!({"planner_provider":"codex","area_id":b.area_id,"admin":false,"theme":{"fg":[255,255,255],"bg":[0,0,0]}});
    assert_eq!(
        b.post_create(Some("admin-key"), body.clone()).await.0,
        StatusCode::CREATED
    );
    body["admin"] = json!(true);
    let (status, error) = b.post_create(Some("admin-key"), body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error.to_string().contains("idempotency_key_reused"));
    assert_eq!(b.track_count().await, 1);
}
