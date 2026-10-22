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

async fn released_receipt() -> SqliteDb {
    let pool = receipt().await;
    sqlx::raw_sql(
        r#"UPDATE acp_submissions SET state='unknown', input_json='{"version":"canonical_v1","association":{}}';
        UPDATE acp_submissions SET kept_cleanup_json='{
          "version":"v1", "worker_session_id":"worker", "client_id":"client",
          "turn_id":"turn", "thread_id":"thread", "native_session_id":"native",
          "association":{}, "registration_digest":"digest", "keep_key":"keep",
          "keep_principal":"owner", "keep_fingerprint":"fingerprint",
          "observation":"stopped_revoked_normally_joined"
        }';
        INSERT INTO worker_sessions VALUES('other');
        INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_json,created_at_ms)
          VALUES('other','other','other','thread','native','{}','completed','{}',1);"#,
    )
    .execute(pool.writer_pool())
    .await
    .unwrap();
    pool
}

async fn receipt_bytes(pool: &SqliteDb) -> Vec<String> {
    sqlx::query_scalar(
        concat!(
        "SELECT json_array(rowid,worker_session_id,client_id,turn_id,thread_id,native_session_id,in",
        "put_json,state,outcome_json,created_at_ms,kept_cleanup_json) FROM acp_submissions ORDER BY",
        " rowid",
    ),
    )
    .fetch_all(pool.read_pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn acp_cleanup_rejects_insert_collisions_without_changing_receipt_bytes() {
    let pool = released_receipt().await;
    let mut db = pool.writer_pool().acquire().await.unwrap();
    // REPLACE must be fenced even when SQLite does not run recursive delete triggers.
    sqlx::query("PRAGMA recursive_triggers=OFF")
        .execute(&mut *db)
        .await
        .unwrap();
    let before = receipt_bytes(&pool).await;
    for (worker, client, turn) in [
        ("worker", "client", "changed-turn"),
        ("other", "changed-client", "turn"),
    ] {
        for evidence in ["", ",kept_cleanup_json"] {
            for conflict in ["OR REPLACE", "", "OR IGNORE"] {
                for suffix in [
                    "",
                    " ON CONFLICT DO UPDATE SET state='completed',outcome_json='{}'",
                ] {
                    let value = if evidence.is_empty() { "" } else { ",NULL" };
                    let sql = format!(
                        concat!(
                            "INSERT {conflict} INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,nativ",
                            "e_session_id,input_json,state,outcome_json,created_at_ms{evidence}) VALUES(?1,?2,?3,'chang",
                            "ed-thread','changed-native','{{}}','completed','{{}}',9{value}){suffix}",
                        ),
                        conflict = conflict,
                        evidence = evidence,
                        value = value,
                        suffix = suffix
                    );
                    assert!(
                        sqlx::query(&sql)
                            .bind(worker)
                            .bind(client)
                            .bind(turn)
                            .execute(&mut *db)
                            .await
                            .is_err(),
                        "{sql}"
                    );
                    assert_eq!(receipt_bytes(&pool).await, before, "{sql}");
                }
            }
        }
    }
}

#[tokio::test]
async fn acp_cleanup_rejects_update_replace_collisions_and_codec_rewrites() {
    let pool = released_receipt().await;
    let before = receipt_bytes(&pool).await;
    for identity in [
        "worker_session_id='worker',client_id='client'",
        "turn_id='turn'",
    ] {
        let sql =
            format!("UPDATE OR REPLACE acp_submissions SET {identity} WHERE client_id='other'");
        assert!(
            sqlx::query(&sql).execute(pool.writer_pool()).await.is_err(),
            "{sql}"
        );
        assert_eq!(receipt_bytes(&pool).await, before);
    }
    // The production codec cannot settle a released Unknown as Completed either.
    assert!(
        acp_submission_finish(&pool, "worker", "client", Some("{}"))
            .await
            .is_err()
    );
    acp_submission_finish(&pool, "worker", "client", None)
        .await
        .unwrap();
    assert_eq!(receipt_bytes(&pool).await, before);
}

#[tokio::test]
async fn acp_cleanup_preserves_unreleased_replace_and_worker_deletion() {
    let pool = released_receipt().await;
    // Only released receipts gain a collision fence.
    for (client, turn) in [("other", "new-turn"), ("new-client", "new-turn")] {
        sqlx::query(concat!(
        "REPLACE INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_",
        "id,input_json,state,outcome_json,created_at_ms) VALUES('other',?1,?2,'thread','native','{}",
        "','completed','{}',2)",
    ))
            .bind(client).bind(turn).execute(pool.writer_pool()).await.unwrap();
    }
    sqlx::query("DELETE FROM worker_sessions WHERE id='worker'")
        .execute(pool.writer_pool())
        .await
        .unwrap();
    assert!(
        super::acp_submission_get(pool.read_pool(), "worker", "client")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(pool.read_pool())
            .await
            .unwrap()
            .is_empty()
    );
}
