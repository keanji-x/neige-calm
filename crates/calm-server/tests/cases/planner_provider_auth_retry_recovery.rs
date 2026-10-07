//! Provider retry shares the original-session recovery entrypoint, never minting a replacement.
use super::planner_preserving_recovery::failed_conversation;
use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use calm_server::auth::{AuthConfig, AuthState};
use calm_server::codex_appserver::Notification;
use tower::ServiceExt;

async fn retry(boot: &Boot) -> (StatusCode, serde_json::Value) {
    let daemon = &boot.state.shared_codex_appserver;
    daemon.emit_notification_for_test(Notification::Other {method:"error".into(),params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used."}})});
    let revision = daemon.authentication_notice().unwrap().revision;
    let auth = AuthState::new(AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Test owner".into(),
    });
    let app = calm_server::routes::application_router(boot.state.clone(), auth);
    let response = app
        .oneshot(
            Request::post("/api/agent-providers/codex/retry")
                .header(header::HOST, "127.0.0.1:4040")
                .header(header::ORIGIN, "http://127.0.0.1:4040")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"expected_revision":revision}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    (status, body)
}
#[tokio::test]
async fn owner_auth_retry_restores_failed_carrier_with_its_original_thread_queue_and_history() {
    let boot = boot_fake_running().await;
    boot.state.shared_codex_appserver.fail_turn_start_for_test();
    let (card, runtime, thread, entry) = failed_conversation(&boot).await;
    let before = conversation_rows(&boot, &card).await;
    let (status, body) = retry(&boot).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["recovery_notices"], json!([]));
    let row = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_ne!(row.status, WorkerSessionState::Failed);
    assert_eq!(row.thread_id.as_deref(), Some(thread.as_str()));
    assert!(
        HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap())
            .pending_entries()
            .contains(&entry)
    );
    let after = conversation_rows(&boot, &card).await;
    for item in before {
        assert!(
            after.iter().any(
                |retained| retained["id"] == item["id"] && retained["params"] == item["params"]
            )
        );
    }
    assert!(
        boot.state
            .shared_codex_appserver
            .started_thread_params_for_test()
            .is_empty()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE card_id=?")
        .bind(card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "auth retry cannot silently rebuild the session");
}
#[tokio::test]
async fn owner_auth_retry_reports_unresumable_carrier_and_preserves_every_durable_field() {
    let boot = boot_fake_running().await;
    let (card, runtime, _, _) = failed_conversation(&boot).await;
    boot.state
        .shared_codex_appserver
        .fail_thread_resume_for_test();
    let before = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    let (status, body) = retry(&boot).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["recovery_notices"][0]["card_id"], card.id.to_string());
    assert_eq!(body["recovery_notices"].as_array().unwrap().len(), 1);
    let after = runtime_by_id_tx_snapshot(&boot.repo, &runtime)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert!(
        boot.state
            .shared_codex_appserver
            .started_thread_params_for_test()
            .is_empty()
    );
}
