//! The compact-kind rebuild keeps the existing table contract and FK-dependent data.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn exec(db: &mut SqliteConnection, sql: &str) {
    sqlx::raw_sql(sql).execute(db).await.unwrap();
}

async fn insert(
    db: &mut SqliteConnection,
    key: &str,
    kind: &str,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) \
         VALUES(?1,'track',?1,?2,'g','{}',?3,1,1)")
        .bind(key).bind(kind).bind(status).execute(db).await.map(|_| ())
}

#[tokio::test]
async fn compact_migration_preserves_rows_refs_schema_and_attempt_contract() {
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
        .filter(|m| m.version < 186 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    for (key, kind, status) in [
        ("pending", "codex", "pending"),
        ("dispatched", "claude", "dispatched"),
        ("running", "terminal", "running"),
        ("verifying", "codex", "verifying"),
        ("done", "claude", "done"),
        ("failed", "codex", "failed"),
        ("canceled", "terminal", "canceled"),
    ] {
        insert(&mut db, key, kind, status).await.unwrap();
        sqlx::query("INSERT INTO task_ref_index VALUES(?1,'other','b_ref')")
            .bind(key)
            .execute(&mut db)
            .await
            .unwrap();
    }
    exec(
        &mut db,
        "UPDATE tasks SET access='read_only', \
         head='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', \
         base='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',start='upstream',running_started_at_ms=5, \
         result_contract='test',child_track_json='{}' WHERE id='running'",
    )
    .await;
    // Snapshot whole rows, not a hand-picked subset of the post-0097 columns.
    let rows = sqlx::query("SELECT * FROM tasks ORDER BY id")
        .fetch_all(&mut db)
        .await
        .unwrap();
    let dump = |rows: &[sqlx::sqlite::SqliteRow]| {
        use sqlx::{Row, ValueRef};
        rows.iter()
            .map(|row| {
                (0..row.len())
                    .map(|i| {
                        let value = row.try_get_raw(i).unwrap();
                        if value.is_null() {
                            None
                        } else {
                            Some(
                                row.try_get::<String, _>(i)
                                    .unwrap_or_else(|_| row.get::<i64, _>(i).to_string()),
                            )
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let before = dump(&rows);
    let objects: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT type,name,sql FROM sqlite_master \
        WHERE (tbl_name IN ('tasks','task_ref_index') AND type IN ('index','trigger') \
               AND sql IS NOT NULL) \
           OR name IN ('current_tasks','task_attempt_recovery_predecessor') ORDER BY name",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    for migration in MIGRATOR.iter().filter(|m| m.version == 186) {
        db.apply(migration).await.unwrap();
    }
    assert_eq!(
        dump(
            &sqlx::query("SELECT * FROM current_tasks ORDER BY id")
                .fetch_all(&mut db)
                .await
                .unwrap()
        ),
        before
    );
    let after_objects: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT type,name,sql FROM sqlite_master \
        WHERE (tbl_name IN ('tasks','task_ref_index') AND type IN ('index','trigger') \
               AND sql IS NOT NULL) \
           OR name IN ('current_tasks','task_attempt_recovery_predecessor') ORDER BY name",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    let normalize = |objects: Vec<(String, String, String)>| {
        objects
            .into_iter()
            .map(|(kind, name, sql)| (kind, name, sql.split_whitespace().collect::<String>()))
            .collect::<Vec<_>>()
    };
    assert_eq!(normalize(after_objects), normalize(objects));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_ref_index")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        7
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_attempt_allocations")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        7
    );
    insert(&mut db, "compact", "compact", "pending")
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT generation FROM task_attempt_allocations WHERE attempt_id='compact'"
        )
        .fetch_one(&mut db)
        .await
        .unwrap(),
        1
    );
    exec(&mut db, "INSERT INTO task_ref_index VALUES('compact','other','b_ref'); UPDATE tasks SET status='failed' WHERE id='compact';").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_ref_index WHERE task_id='compact'")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        0
    );
    assert!(insert(&mut db, "bogus", "bogus", "pending").await.is_err());
    // The predecessor trigger and composite FK still admit a real second generation.
    exec(&mut db, r#"INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms)
        VALUES('recovered','track','failed',2,'{"kind":"recovery","previous_attempt_id":"failed","idempotency_key":"recover","request_fingerprint":"fingerprint","reason":"retry","actor":{},"constraint":{"version":"v1","spawn":"in-wave","declared_by":"spec","refs":["ref"]}}',2);
        INSERT INTO tasks(id,track_id,key,kind,goal,context_json,created_at_ms,updated_at_ms)
        VALUES('recovered','track','failed','codex','retry','{}',2,2);"#).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM current_tasks WHERE key='failed'")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        "recovered"
    );
    let bad_fk = sqlx::query("UPDATE tasks SET track_id='other' WHERE id='recovered'")
        .execute(&mut db)
        .await;
    assert!(bad_fk.is_err());
    // Restored task_ref_index still cascades, and all inbound FKs still resolve.
    exec(&mut db, "DELETE FROM tasks WHERE id='pending'").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_ref_index WHERE task_id='pending'")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        0
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
}
