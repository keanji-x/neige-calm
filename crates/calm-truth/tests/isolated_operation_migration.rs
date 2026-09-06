use calm_truth::MIGRATOR;
use serde_json::{Value, json};
use sqlx::{Connection, Row, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn prior_schema() -> SqliteConnection {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version <= 98 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);
        INSERT INTO cards(id,track_id,kind,role,sort,created_at,updated_at) VALUES('card','track','codex','worker',0,1,2);").execute(&mut db).await.unwrap();
    db
}

async fn operation(
    db: &mut SqliteConnection,
    id: &str,
    kind: &str,
    phase: &str,
    artifacts: Option<&str>,
    receipt: Option<&Value>,
) {
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms,spawn_artifacts_json,parked_at_ms,parked_deadline_ms)
        VALUES(?1,?1,?2,?1,'hash','card','card','{}','{}',?3,?4,11,12,?5,13,14)")
        .bind(id).bind(kind).bind(receipt.map(Value::to_string)).bind(phase).bind(artifacts)
        .execute(db).await.unwrap();
}

fn receipt(id: &str) -> Value {
    json!({"data":{"isolated_execution":{
        "version":"isolated-run-v1", "request":{"identity":{"run_id":id,"attempt_id":id,"card_id":"card"}},
        "provider":{"state":"prepared","record":{"endpoint":{"version":2,"boundary":{"attempt_id":id}},
        "phase":{"TurnActive":{"thread_id":"thread","turn_id":"turn"}}}}
    }}})
}

async fn all_rows(db: &mut SqliteConnection, table: &str) -> Vec<String> {
    assert!(matches!(table, "operations" | "worker_sessions"));
    let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(&mut *db)
        .await
        .unwrap();
    let columns = columns
        .iter()
        .map(|row| {
            let name: String = row.get("name");
            assert!(name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
            format!("\"{name}\"")
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query_scalar(&format!(
        "SELECT json_array({columns}) FROM {table} ORDER BY id"
    ))
    .fetch_all(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn isolated_parked_upgrade_preserves_rows_references_and_keyed_fence() {
    let mut db = prior_schema().await;
    operation(
        &mut db,
        "legacy",
        "codex-worker",
        "parked",
        Some("{\"preserve\":1}"),
        None,
    )
    .await;
    operation(
        &mut db,
        "isolated",
        "codex-isolated-worker",
        "spawn_started",
        None,
        Some(&receipt("isolated")),
    )
    .await;
    sqlx::query("INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,spawn_op_id,card_id,created_at_ms,updated_at_ms)
        VALUES('session','track','codex','resumable','executor','starting','legacy','card',21,22)")
        .execute(&mut db).await.unwrap();
    // Pin the referencing-table inventory at the released pre-upgrade schema.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    let mut references = Vec::new();
    for table in tables {
        for fk in sqlx::query(&format!(
            "PRAGMA foreign_key_list(\"{}\")",
            table.replace('"', "\"\"")
        ))
        .fetch_all(&mut db)
        .await
        .unwrap()
        {
            if fk.get::<String, _>("table") == "operations" {
                references.push((table.clone(), fk.get::<String, _>("from")));
            }
        }
    }
    assert_eq!(
        references,
        vec![("worker_sessions".into(), "spawn_op_id".into())]
    );
    let old_operations = all_rows(&mut db, "operations").await;
    let old_sessions = all_rows(&mut db, "worker_sessions").await;
    // Reproduce the released constraint against a correctly selected owned operation.
    assert!(
        sqlx::query("UPDATE operations SET phase='parked' WHERE id='isolated'")
            .execute(&mut db)
            .await
            .is_err()
    );
    MIGRATOR.run(&mut db).await.unwrap();
    assert_eq!(all_rows(&mut db, "operations").await, old_operations);
    assert_eq!(all_rows(&mut db, "worker_sessions").await, old_sessions);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        1
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        sqlx::query("DELETE FROM operations")
            .execute(&mut db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM operations WHERE id='isolated'")
            .execute(&mut db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE worker_sessions SET spawn_op_id='missing'")
            .execute(&mut db)
            .await
            .is_err()
    );
    sqlx::query("UPDATE operations SET phase='parked' WHERE id='isolated'")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE operations SET spawn_artifacts_json=NULL WHERE id='legacy'")
            .execute(&mut db)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn isolated_parked_schema_rejects_missing_or_mismatched_receipts() {
    let mut db = prior_schema().await;
    MIGRATOR.run(&mut db).await.unwrap();
    let good = receipt("isolated");
    operation(
        &mut db,
        "isolated",
        "codex-isolated-worker",
        "parked",
        None,
        Some(&good),
    )
    .await;
    let mut bad = vec![Value::Null, json!({})];
    for (pointer, value) in [
        ("/data/isolated_execution/version", json!("unknown")),
        (
            "/data/isolated_execution/request/identity/run_id",
            json!("foreign"),
        ),
        (
            "/data/isolated_execution/request/identity/attempt_id",
            json!("foreign"),
        ),
        (
            "/data/isolated_execution/request/identity/card_id",
            json!("foreign"),
        ),
        (
            "/data/isolated_execution/provider/state",
            json!("unprepared"),
        ),
        (
            "/data/isolated_execution/provider/record/endpoint/version",
            json!(1),
        ),
        (
            "/data/isolated_execution/provider/record/endpoint/boundary/attempt_id",
            json!("foreign"),
        ),
        (
            "/data/isolated_execution/provider/record/phase",
            json!("CreatingThread"),
        ),
    ] {
        let mut changed = good.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        bad.push(changed);
    }
    for value in bad {
        assert!(
            sqlx::query("UPDATE operations SET tx_output_json=? WHERE id='isolated'")
                .bind(value.to_string())
                .execute(&mut db)
                .await
                .is_err()
        );
    }
    assert!(
        sqlx::query("UPDATE operations SET tx_output_json=NULL WHERE id='isolated'")
            .execute(&mut db)
            .await
            .is_err()
    );
    for mutation in [
        "target_id='foreign'",
        "idempotency_key=NULL",
        "spawn_artifacts_json='{}'",
        "parked_at_ms=NULL",
        "parked_deadline_ms=NULL",
        "kind='codex-worker'",
    ] {
        assert!(
            sqlx::query(&format!(
                "UPDATE operations SET {mutation} WHERE id='isolated'"
            ))
            .execute(&mut db)
            .await
            .is_err(),
            "{mutation}"
        );
    }
    let stored: String =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id='isolated'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), good);
}
