//! Settlement CAS against the released receipt schema, without unrelated owner fixtures.
use super::acp_submission_finish;
use crate::error::TruthError;
use calm_types::error::CoreError;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

async fn receipt() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().foreign_keys(false))
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0157_acp_submission_journal.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO acp_submissions VALUES ('worker','client','turn','thread','native','{}','sending',NULL,0)")
        .execute(&pool).await.unwrap();
    pool
}
fn conflict(result: Result<(), TruthError>) {
    assert!(
        matches!(result, Err(TruthError::Core(CoreError::Conflict(_)))),
        "{result:?}"
    );
}

#[tokio::test]
async fn acp_submission_settle_exact_idempotency_and_conflicts() {
    let pool = receipt().await;
    let outcome = r#"{ "status": "completed", "items": [] }"#;
    acp_submission_finish(&pool, "worker", "client", Some(outcome))
        .await
        .unwrap();
    acp_submission_finish(&pool, "worker", "client", Some(outcome))
        .await
        .unwrap();
    for different in [
        None,
        Some(r#"{"status":"completed","items":[]}"#),
        Some(r#"{"status":"failed"}"#),
    ] {
        conflict(acp_submission_finish(&pool, "worker", "client", different).await);
    }
    conflict(acp_submission_finish(&pool, "other-worker", "client", Some(outcome)).await);
    conflict(acp_submission_finish(&pool, "worker", "other-client", Some(outcome)).await);
    let stored: String = sqlx::query_scalar("SELECT outcome_json FROM acp_submissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, outcome);
}

#[tokio::test]
async fn acp_submission_settle_unknown_can_complete() {
    let pool = receipt().await;
    acp_submission_finish(&pool, "worker", "client", None)
        .await
        .unwrap();
    // Boot recovery may encounter the same unknown receipt again.
    acp_submission_finish(&pool, "worker", "client", None)
        .await
        .unwrap();
    acp_submission_finish(&pool, "worker", "client", Some("{}"))
        .await
        .unwrap();
    let row = super::acp_submission_get(&pool, "worker", "client")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(row.state, super::AcpSubmissionState::Completed));
    assert_eq!(row.outcome_json.as_deref(), Some("{}"));
}
