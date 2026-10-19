use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::borrow::Cow;

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}
async fn pool(version: i64) -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    migrator_through(version).run(&pool).await.unwrap();
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
async fn rows(pool: &SqlitePool, table: &str) -> Vec<String> {
    let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .unwrap();
    let args = columns
        .iter()
        .flat_map(|r| {
            let name: String = r.get("name");
            [format!("'{name}'"), name]
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query_scalar(&format!(
        "SELECT json_object({args}) FROM {table} ORDER BY id"
    ))
    .fetch_all(pool)
    .await
    .unwrap()
}
async fn schema(pool: &SqlitePool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT name,sql FROM sqlite_master WHERE tbl_name='operations' AND type IN \
            ('index','trigger') AND sql IS NOT NULL ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}
#[tokio::test]
async fn upgrade_preserves_rows_refs_indexes_and_keyed_fence() {
    let pool = pool(173).await;
    insert(&pool, "native", "task-gate-run").await;
    insert(&pool, "isolated", "codex-isolated-worker").await;
    insert(&pool, "retry", "test").await;
    sqlx::query("UPDATE operations SET spawn_artifacts_json='{}' WHERE id='native'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET target_id='card',tx_output_json=?1 WHERE id='isolated'")
        .bind(RECEIPT)
        .execute(&pool)
        .await
        .unwrap();
    park(&pool, "native", Some(3), Some(4)).await.unwrap();
    park(&pool, "isolated", Some(3), Some(4)).await.unwrap();
    sqlx::query("UPDATE operations SET phase='awaiting_retry',tx_output_json='{}',phase_detail_json='{\"kind\":\"mint_and_await\"}' WHERE id='retry'").execute(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('a','a','#000',0,1,1); \
            INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('t','a','t',0,1,1);").execute(&pool).await.unwrap();
    for id in ["native", "isolated", "retry"] {
        sqlx::query("INSERT INTO \
            worker_sessions(id,track_id,provider,mode,contract,state,spawn_op_id,created_at_ms,updated_at_ms) \
            VALUES(?1,'t','terminal','ephemeral','executor','running',?1,1,2)").bind(id).execute(&pool).await.unwrap();
    }
    let before = (
        rows(&pool, "operations").await,
        rows(&pool, "worker_sessions").await,
        schema(&pool).await,
    );
    migrator_through(174).run(&pool).await.unwrap();
    assert_eq!(
        (
            rows(&pool, "operations").await,
            rows(&pool, "worker_sessions").await,
            schema(&pool).await
        ),
        before
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        sqlx::query("DELETE FROM operations WHERE id='retry'")
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string()
            .contains("submit() dedup wall")
    );
}
#[tokio::test]
async fn parked_check_keeps_only_owner_neutral_facts() {
    let pool = pool(174).await;
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
