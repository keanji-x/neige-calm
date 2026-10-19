//! A declared AI label changes attribution only after real HTTP authentication.
use super::*;

#[tokio::test]
async fn authenticated_ai_labelled_send_records_live_planner_session_identity() {
    let boot = boot().await;
    let (card, worker_session_id, harness) = seed_live_planner_harness(&boot).await;
    // The authenticated router supplies its actual password-login cookie when
    // none is specified. Keep the AI label, without the invalid-cookie helper.
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/planner/input", card.id))
                .header("content-type", "application/json")
                .header("idempotency-key", new_id())
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::from(
                    json!({"text":"authenticated labelled send"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM events WHERE kind='harness.user_message.enqueued' ORDER BY id DESC LIMIT 1"
    ).fetch_one(boot.repo.pool()).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&actor).unwrap(),
        json!({"kind":"AiPlannerSession","id":worker_session_id})
    );
    shutdown_seeded_harness(&boot, &worker_session_id, harness).await;
}
