//! Metadata ensure is independent of the Planner's start lock.
use crate::planner_first_start::{AppServer, app_state, router};
use crate::planner_repoint_restart_lock::request;
use axum::http::StatusCode;
use serde_json::json;
#[tokio::test]
async fn u11_canonical_today_ensure_does_not_wait_for_or_restart_native_lifecycle() {
    let (_tmp, repo, state) = app_state(AppServer::Running).await;
    let app = router(&state).await;
    let (status, dto) = request(
        app.clone(),
        "POST",
        "/api/today/launchpad/ensure".into(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let _lock = state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let before = crate::today_launchpad_adoption::database(&repo).await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        request(app, "POST", "/api/today/launchpad/ensure".into(), json!({})),
    )
    .await
    .expect("ensure does not need execution authority");
    assert_eq!(result, (StatusCode::OK, dto));
    assert_eq!(
        crate::today_launchpad_adoption::database(&repo).await,
        before
    );
}
