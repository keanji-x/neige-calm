use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

fn migrator_through_0066() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 66)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

fn migrator_through_0068() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 68)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

fn migrator_through_0069() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 69)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[tokio::test]
async fn upgrade_backfills_legacy_nonterminal_claim_context_to_empty_set() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through_0066()
        .run(&pool)
        .await
        .expect("apply migrations through 0066");

    sqlx::query(
        r#"INSERT INTO tasks (
             id, wave_id, key, kind, goal, context_json, depends_on_json,
             status, created_at_ms, updated_at_ms
           ) VALUES (
             'wave-legacy:in-flight', 'wave-legacy', 'in-flight', 'codex',
             'legacy work', 'null', '[]', 'running', 1, 1
           )"#,
    )
    .execute(&pool)
    .await
    .expect("seed task created before context columns existed");

    crate::MIGRATOR
        .run(&pool)
        .await
        .expect("upgrade through context-freeze migration");
    let claim_context: Option<String> =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
            .bind("wave-legacy:in-flight")
            .fetch_one(&pool)
            .await
            .expect("read upgraded legacy task");
    assert_eq!(claim_context.as_deref(), Some("[]"));

    // The first correctness sweep treats a present empty closure as a
    // verified legacy claim, not as the fail-closed "snapshot missing"
    // case. Pin both halves of that distinction at the upgrade boundary.
    let frozen = claim_context
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<calm_types::event::TaskContextRef>>(json).ok());
    assert_eq!(frozen, Some(Vec::new()));
    let first_sweep_material = frozen.is_none();
    assert!(
        !first_sweep_material,
        "the first sweep after upgrade must not mark a backfilled legacy task material"
    );
}

#[tokio::test]
async fn pending_context_stale_cleanup_is_idempotent_and_scoped() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through_0068()
        .run(&pool)
        .await
        .expect("apply migrations through 0068");
    sqlx::query(
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms,context_stale_at_ms) VALUES ('p','w','p','codex','p','null','[]','pending',1,1,9), ('r','w','r','codex','r','null','[]','running',1,1,9)",
    )
    .execute(&pool)
    .await
    .expect("seed stale rows");

    let events_before: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&pool)
        .await
        .expect("count events before migration");
    migrator_through_0069()
        .run(&pool)
        .await
        .expect("apply real 0069 migration");
    let repeated = sqlx::query(include_str!(
        "../../../migrations/0069_clear_pending_context_stale.sql"
    ))
    .execute(&pool)
    .await
    .expect("execute the 0069 statement body a second time");
    assert_eq!(
        repeated.rows_affected(),
        0,
        "the 0069 SQL body itself must be idempotent"
    );
    let pending_stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = 'p'")
            .fetch_one(&pool)
            .await
            .expect("read pending row");
    assert_eq!(pending_stale, None);
    let stale: Option<i64> =
        sqlx::query_scalar("SELECT context_stale_at_ms FROM tasks WHERE id = 'r'")
            .fetch_one(&pool)
            .await
            .expect("read in-flight row");
    assert_eq!(
        stale,
        Some(9),
        "migration must not touch in-flight verdicts"
    );
    let events_after: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&pool)
        .await
        .expect("count events after migration");
    assert_eq!(
        events_after, events_before,
        "migration must not emit events"
    );
}

#[tokio::test]
async fn upgrade_0070_backfills_inflight_block_declaration_state() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through_0069()
        .run(&pool)
        .await
        .expect("apply migrations through 0069");
    sqlx::query(
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,declared_by,origin,claim_context_json,created_at_ms,updated_at_ms) VALUES \
         ('flight','w','flight','codex','g','null','[]','running','spec','block','[]',1,1), \
         ('pending','w','pending','codex','g','null','[]','pending','spec','block',NULL,1,1), \
         ('terminal','w','terminal','codex','g','null','[]','done','spec','block','[]',1,1), \
         ('legacy','w','legacy','codex','g','null','[]','running','spec','legacy','[]',1,1)",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0070 in-flight block task");

    crate::MIGRATOR
        .run(&pool)
        .await
        .expect("run the real 0070 migration");
    let rows: Vec<(String, i64, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id,decl_ready,decl_released_by_user,context_verify_failures,context_stale_at_ms FROM tasks ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("read upgraded tasks");
    assert_eq!(
        rows,
        vec![
            ("flight".into(), 1, 0, 0, None),
            ("legacy".into(), 0, 0, 0, None),
            ("pending".into(), 0, 0, 0, None),
            ("terminal".into(), 0, 0, 0, None),
        ]
    );

    let migration = include_str!("../../../migrations/0070_task_context_withdrawal_and_verify.sql");
    let start = migration
        .find("UPDATE tasks SET decl_ready")
        .expect("0070 contains decl_ready backfill");
    let backfill = migration[start..]
        .split(';')
        .next()
        .expect("0070 terminates decl_ready backfill");
    let repeated = sqlx::query(backfill)
        .execute(&pool)
        .await
        .expect("execute the backfill from the real 0070 migration file a second time");
    assert_eq!(repeated.rows_affected(), 0, "0070 backfill is idempotent");
}
