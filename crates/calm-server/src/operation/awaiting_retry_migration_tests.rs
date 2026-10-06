//! Upgrade the released schema with FK enforcement and permanent keyed rows intact.
use std::borrow::Cow;

use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn snapshot(pool: &SqlitePool) -> Vec<String> {
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('operations') ORDER BY cid")
            .fetch_all(pool)
            .await
            .unwrap();
    let pairs = columns
        .iter()
        .map(|column| format!("'{column}',\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query_scalar(&format!(
        "SELECT json_object({pairs}) FROM operations ORDER BY id"
    ))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn awaiting_retry_migration_preserves_rows_session_refs_indices_and_parked_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let options = SqliteConnectOptions::new()
        .filename(temp.path().join("before.db"))
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let previous = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|migration| migration.version < 152)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    previous.run(&pool).await.unwrap();
    let repo = SqlxOperationRepo::new(pool.clone());
    for name in ["active", "complete", "parked"] {
        let id = repo
            .insert_operation(
                "existing-kind",
                OperationKey {
                    operation_key: name.into(),
                    idempotency_key: Some(name.into()),
                    payload_hash: name.into(),
                },
                json!({"identity":name}),
            )
            .await
            .unwrap();
        if name == "complete" {
            sqlx::query("UPDATE operations SET phase='succeeded', completed_at_ms=42 WHERE id=?1")
                .bind(&id)
                .execute(&pool)
                .await
                .unwrap();
        }
        if name == "parked" {
            sqlx::query("UPDATE operations SET phase='parked', parked_at_ms=1, parked_deadline_ms=999, spawn_artifacts_json=?1 WHERE id=?2")
                .bind(json!({"pid":1,"pgid":1,"start_time":1,"boot_id":"old","log_path":null,"extra":null}).to_string())
                .bind(&id).execute(&pool).await.unwrap();
        }
    }
    let operation_id: String =
        sqlx::query_scalar("SELECT id FROM operations WHERE operation_key='active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','area','#000',0,1,1)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','track',0,1,1)").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO cards(id,track_id,kind,sort,payload,created_at,updated_at,role)
         VALUES('card','track','codex',0,'{}',1,1,'worker')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,spawn_op_id,created_at_ms,updated_at_ms)
         VALUES('session','track','codex','resumable','planner','failed',?1,1,1)")
        .bind(&operation_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE cards SET session_id='session' WHERE id='card'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    calm_truth::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(
        snapshot(&pool).await,
        before,
        "every existing operation column remains byte-equivalent"
    );
    let refs:(Option<String>,Option<String>) = sqlx::query_as("SELECT ws.spawn_op_id,c.session_id FROM worker_sessions ws JOIN cards c ON c.id='card' WHERE ws.id='session'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(refs, (Some(operation_id.clone()), Some("session".into())));
    let foreign_key_failures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(foreign_key_failures, 0);
    let indices: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='operations' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for expected in [
        "operations_kind_idempotency_key_unique",
        "operations_drive_scan_idx",
        "operations_target_idx",
    ] {
        assert!(
            indices.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
    assert!(
        sqlx::query("DELETE FROM operations WHERE id=?1")
            .bind(&operation_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        repo.insert_operation(
            "existing-kind",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("active".into()),
                payload_hash: "changed".into()
            },
            json!({})
        )
        .await
        .is_err()
    );
    assert!(
        sqlx::query("UPDATE operations SET phase='parked' WHERE id=?1")
            .bind(&operation_id)
            .execute(&pool)
            .await
            .is_err(),
        "legacy parked evidence is still mandatory"
    );
    assert!(
        sqlx::query("UPDATE operations SET phase='awaiting_retry' WHERE id=?1")
            .bind(&operation_id)
            .execute(&pool)
            .await
            .is_err(),
        "a deferred phase needs a saved continuation and receipt"
    );
}
