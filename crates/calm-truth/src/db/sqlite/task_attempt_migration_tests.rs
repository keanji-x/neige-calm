//! Migration operates on a real released schema with foreign keys enabled.
use std::borrow::Cow;

use super::SqlxRepo;
use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;
use sqlx::sqlite::SqlitePoolOptions;

async fn snapshot(pool: &sqlx::SqlitePool, table: &str, order: &str) -> Vec<String> {
    let columns: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .fetch_all(pool)
    .await
    .unwrap();
    sqlx::query_scalar(&format!(
        "SELECT json_array({}) FROM {table} ORDER BY {order}",
        columns.join(",")
    ))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn task_recovery_migration_preserves_all_execution_values_refs_and_operations() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("old.sqlite").display()
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    let migrator = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 93)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    migrator.run(&pool).await.unwrap();
    for status in [
        "pending",
        "dispatched",
        "running",
        "verifying",
        "done",
        "failed",
        "canceled",
    ] {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms,claim_context_json,spawn,child_track_id,gate_attempt,gate_pid,gate_pid_starttime,gate_pid_boot_id,decl_ready,decl_released_by_user,context_verify_failures,context_stale_at_ms,context_closure_truncated) VALUES(?1,'legacy',?2,'claude','old goal','{\"source\":\"old\"}','[\"dependency\"]',?2,10,11,'[]',?3,?1,3,123,456,'old-boot',1,1,2,888,1)")
            .bind(format!("nonconventional-{status}")).bind(status).bind(TASK_IN_TRACK_ROUTE).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO task_ref_index VALUES('nonconventional-running','other','b_ref'),('nonconventional-verifying','legacy','b_root')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES('op','operation-key','codex-worker','nonconventional-running','old-hash','card','{}','{\"frozen\":true}','{\"kept\":1}','pending',5,6)")
        .execute(&pool).await.unwrap();
    let tasks_before = snapshot(&pool, "tasks", "id").await;
    let refs_before = snapshot(&pool, "task_ref_index", "task_id").await;
    let operations_before = snapshot(&pool, "operations", "id").await;
    pool.close().await;
    let repo = SqlxRepo::open(&url)
        .await
        .expect("real startup migrates with FK ON");
    assert_eq!(snapshot(repo.pool(), "tasks", "id").await, tasks_before);
    assert_eq!(
        snapshot(repo.pool(), "task_ref_index", "task_id").await,
        refs_before
    );
    assert_eq!(
        snapshot(repo.pool(), "operations", "id").await,
        operations_before
    );
    let allocations:Vec<(String,String,i64,String)>=sqlx::query_as("SELECT attempt_id,key,generation,origin_json FROM task_attempt_allocations ORDER BY attempt_id")
        .fetch_all(repo.pool()).await.unwrap();
    assert_eq!(allocations.len(), 7);
    for (attempt, key, generation, origin) in allocations {
        assert_eq!(attempt, format!("nonconventional-{key}"));
        assert_eq!(generation, 1);
        assert_eq!(origin, "{\"kind\":\"initial\"}");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='tasks'",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    for name in [
        "tasks_track_status_idx",
        "idx_tasks_liveness_deadlines",
        "idx_tasks_child_track_id",
    ] {
        assert!(indexes.iter().any(|index| index == name), "missing {name}");
    }
    sqlx::query("UPDATE tasks SET status='done' WHERE id='nonconventional-running'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM task_ref_index WHERE task_id='nonconventional-running'"
        )
        .fetch_one(repo.pool())
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM task_ref_index WHERE task_id='nonconventional-verifying'"
        )
        .fetch_one(repo.pool())
        .await
        .unwrap(),
        1
    );
}
