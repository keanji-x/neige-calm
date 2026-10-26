//! Ensure allocates one empty carrier without entering provider lifecycle.
use crate::planner_first_start::{AppServer, app_state, router};
use crate::planner_repoint_restart_lock::{joined, request};
use axum::http::StatusCode;
use calm_server::db::sqlite::SqlxRepo;
use serde_json::{Value, json};
fn ensure(app: axum::Router) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    tokio::spawn(request(
        app,
        "POST",
        "/api/today/launchpad/ensure".into(),
        json!({}),
    ))
}
async fn scalar(repo: &SqlxRepo, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(repo.pool())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
/// Every start this database holds; the launchpad's Planner is the only harness card here.
async fn start_ops(repo: &SqlxRepo) -> i64 {
    scalar(
        repo,
        "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start'",
    )
    .await
}
async fn launchpad_planner(repo: &SqlxRepo) -> String {
    sqlx::query_scalar(
        "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
          WHERE t.purpose = 'launchpad' AND c.role = 'planner'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn u11_canonical_today_concurrent_ensure_has_one_empty_carrier() {
    let (_tmp, repo, state) = app_state(AppServer::Running).await;
    let app = router(&state).await;
    let (a, b) = tokio::join!(
        joined(ensure(app.clone()), "first"),
        joined(ensure(app), "second")
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CREATED]);
    assert_eq!(a.1, b.1);
    assert_eq!(start_ops(&repo).await, 0);
    let sql = "SELECT COUNT(*) FROM worker_sessions WHERE contract='planner'";
    assert_eq!(scalar(&repo, sql).await, 1);
    assert_eq!(scalar(&repo, "SELECT COUNT(*) FROM tracks").await, 1);
}

#[tokio::test]
async fn u11_canonical_today_empty_carrier_without_start_or_input() {
    let (_tmp, repo, state) = app_state(AppServer::Running).await;
    let (status, body) = joined(ensure(router(&state).await), "empty ensure").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["planner_card_id"], launchpad_planner(&repo).await);
    assert_eq!(start_ops(&repo).await, 0);
    let (snapshot, thread): (String, Option<String>) = sqlx::query_as(
        "SELECT ws.handle_state_json,ws.thread_id FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id WHERE c.id=?1",
    ).bind(body["planner_card_id"].as_str().unwrap()).fetch_one(repo.pool()).await.unwrap();
    let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["schema_version"], 5);
    assert_eq!(
        snapshot["canonical_inputs"],
        serde_json::to_value(calm_types::conversation::ConversationInputCheckpoint::empty())
            .unwrap()
    );
    assert!(thread.is_none());
    assert_eq!(scalar(&repo, "SELECT COUNT(*) FROM harness_items").await, 0);
}
