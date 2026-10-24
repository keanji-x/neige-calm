use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    crate::MIGRATOR.run(&pool).await.unwrap();
    pool
}
async fn insert(pool: &SqlitePool, id: &str, kind: &str) {
    sqlx::query("INSERT INTO \
            operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) \
            VALUES(?1,?1,?2,?1,'hash','card','{}','{}','spawn_started',1,2)")
        .bind(id).bind(kind).execute(pool).await.unwrap();
}
async fn park(
    pool: &SqlitePool,
    id: &str,
    at: Option<i64>,
    deadline: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE operations SET phase='parked',parked_at_ms=?2,parked_deadline_ms=?3 WHERE id=?1",
    )
    .bind(id)
    .bind(at)
    .bind(deadline)
    .execute(pool)
    .await
    .map(|_| ())
}
const RECEIPT: &str = r#"{"data":{"isolated_execution":{"version":"isolated-run-v1","request":{"identity":{"run_id":"isolated","attempt_id":"isolated","card_id":"card"}},"provider":{"state":"prepared","record":{"endpoint":{"version":2,"boundary":{"attempt_id":"isolated"}},"phase":{"TurnActive":{}}}}}}}"#;
#[tokio::test]
async fn parked_check_keeps_only_owner_neutral_facts() {
    let pool = pool().await;
    for kind in ["task-gate-run", "task-verify"] {
        insert(&pool, kind, kind).await;
        assert!(park(&pool, kind, None, Some(4)).await.is_err());
        assert!(park(&pool, kind, Some(3), None).await.is_err());
        park(&pool, kind, Some(3), Some(4)).await.unwrap();
    }
    insert(&pool, "isolated", "codex-isolated-worker").await;
    assert!(park(&pool, "isolated", Some(3), Some(4)).await.is_err());
    sqlx::query("UPDATE operations SET target_id='card',tx_output_json=?1 WHERE id='isolated'")
        .bind(RECEIPT)
        .execute(&pool)
        .await
        .unwrap();
    park(&pool, "isolated", Some(3), Some(4)).await.unwrap();
    for path in [
        "$.data.isolated_execution.version",
        "$.data.isolated_execution.provider.record.phase.TurnActive",
    ] {
        assert!(sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,?1) WHERE id='isolated'").bind(path).execute(&pool).await.is_err());
    }
}
