//! #1829 — the transcript wire carries a failed turn's `error.message` in readable form as
//! `turn_error_text`, reduced by the same function as the planner-down item; `params` keeps the raw
//! message, and every other row says `null`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::track_activity::ActivityWake;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use super::track_activity_fixture::fx;
use super::track_notification_dismissals::authenticated_app;
use super::track_notifications::{codex_planner, turn};

#[tokio::test]
async fn a_failed_turn_row_carries_its_readable_error_text() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    // A real Planner card: a codex card whose payload names its provider (the route serves only those).
    sqlx::query("UPDATE cards SET kind = 'codex', payload = '{\"planner_provider\":\"codex\"}' WHERE id = ?1")
        .bind(&p.card)
        .execute(&f.pool)
        .await
        .unwrap();
    let raw = r#"{"type":"error","status":400,"error":{"message":"Upgrade Codex."}}"#;
    turn(&f, &p, "turn-1", "completed", None).await;
    turn(&f, &p, "turn-2", "failed", Some(raw)).await;
    let response = authenticated_app(&f, ActivityWake::detached())
        .await
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/cards/{}/harness/items?after_id=0&limit=50&direction=asc",
                    p.card
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        rows[0]["turn_error_text"],
        Value::Null,
        "a completed turn has none"
    );
    assert_eq!(rows[1]["turn_error_text"], "400: Upgrade Codex.");
    let params: Value = serde_json::from_str(rows[1]["params"].as_str().unwrap()).unwrap();
    assert_eq!(
        params["error"]["message"], raw,
        "params keeps the raw message"
    );
}
