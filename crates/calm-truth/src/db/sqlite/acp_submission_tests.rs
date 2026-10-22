//! Settlement CAS against the released receipt schema, without unrelated owner fixtures.
use super::SqliteDb;
use super::acp_submission_finish;
use crate::error::TruthError;
use calm_types::error::CoreError;

async fn receipt() -> SqliteDb {
    let pool = SqliteDb::connect("sqlite::memory:").await.unwrap();
    // The released receipt migration refers only to this parent key. Keep the
    // CAS fixture small while exercising the production factory's FK contract.
    sqlx::raw_sql("CREATE TABLE worker_sessions(id TEXT PRIMARY KEY); INSERT INTO worker_sessions VALUES('worker');")
        .execute(pool.writer_pool()).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0157_acp_submission_journal.sql"
    ))
    .execute(pool.writer_pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO acp_submissions VALUES ('worker','client','turn','thread','native','{}','sending',NULL,0)")
        .execute(pool.writer_pool()).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0187_acp_kept_cleanup.sql"
    ))
    .execute(pool.writer_pool())
    .await
    .unwrap();
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
        .fetch_one(pool.read_pool())
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
    let row = super::acp_submission_get(pool.read_pool(), "worker", "client")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(row.state, super::AcpSubmissionState::Completed));
    assert_eq!(row.outcome_json.as_deref(), Some("{}"));
}
