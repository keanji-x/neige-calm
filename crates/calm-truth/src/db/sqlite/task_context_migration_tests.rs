use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

use super::SqlxRepo;

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

fn migrator_through_0072() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 72)
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

    migrator_through_0072()
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
async fn upgrade_0068_backfills_preexisting_tasks_as_spec_declared() {
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
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms) \
         VALUES ('preexisting','w','preexisting','codex','g','null','[]','done',1,1)",
    )
    .execute(&pool)
    .await
    .expect("seed task created before declared_by existed");

    migrator_through_0068()
        .run(&pool)
        .await
        .expect("apply migrations through 0068");
    let declared_by: String =
        sqlx::query_scalar("SELECT declared_by FROM tasks WHERE id='preexisting'")
            .fetch_one(&pool)
            .await
            .expect("read 0068 attribution backfill");
    assert_eq!(
        declared_by, "spec",
        "0068 must put every preexisting task in the spec occupancy domain"
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
         ('terminal','w','terminal','codex','g','null','[]','done','spec','block','[]',1,1)",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0070 in-flight block task");

    migrator_through_0072()
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

#[tokio::test]
async fn sqlx_repo_open_rejects_nonterminal_legacy_rows_at_0073() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let path = dir.path().join("nonterminal-at-0072.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("open migration fixture");
    migrator_through_0072()
        .run(&pool)
        .await
        .expect("apply migrations through 0072");
    sqlx::query(
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,declared_by,origin,claim_context_json,spawn,decl_ready,decl_released_by_user,context_verify_failures,created_at_ms,updated_at_ms) \
         VALUES ('in-flight','w','in-flight','codex','g','null','[]','running','spec','legacy','[]','in-wave',0,0,0,1,1)",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0073 nonterminal legacy task");
    pool.close().await;

    let result = SqlxRepo::open(&url).await;
    assert!(
        result.is_err(),
        "the production startup boundary must reject a nonterminal legacy task at 0073"
    );
}

#[tokio::test]
async fn upgrade_from_pre_0068_with_nonterminal_task_aborts_cleanly_at_0072() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let path = dir.path().join("pre-0068-nonterminal.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("open migration fixture");
    migrator_through_0066()
        .run(&pool)
        .await
        .expect("apply migrations through 0066");
    sqlx::query(
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms) \
         VALUES ('preexisting-live','w','preexisting-live','codex','g','null','[]','running',1,1)",
    )
    .execute(&pool)
    .await
    .expect("seed nonterminal task before origin existed");
    migrator_through_0072()
        .run(&pool)
        .await
        .expect("upgrade the pre-0068 fixture through 0072");
    pool.close().await;

    assert!(
        SqlxRepo::open(&url).await.is_err(),
        "0073 must fail closed when 0068 attributed a preexisting live task"
    );

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("reopen the rejected database for inspection");
    let highest: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read migration stop");
    assert_eq!(highest, 72, "failed 0073 must not be recorded as applied");
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks')")
        .fetch_all(&pool)
        .await
        .expect("read task columns after rejected migration");
    assert!(
        columns.iter().any(|column| column == "origin"),
        "the failed migration must roll back DROP COLUMN origin"
    );
    let origin: String = sqlx::query_scalar("SELECT origin FROM tasks WHERE id='preexisting-live'")
        .fetch_one(&pool)
        .await
        .expect("read the row preserved by the rejected migration");
    assert_eq!(origin, "legacy", "0068 must attribute the preexisting row");
}

#[tokio::test]
async fn sqlx_repo_open_accepts_nonterminal_block_row_at_0073_without_data_loss() {
    const HEAD_TASK_COLUMNS: &[&str] = &[
        "id",
        "wave_id",
        "key",
        "kind",
        "goal",
        "context_json",
        "acceptance_criteria",
        "cwd",
        "depends_on_json",
        "priority",
        "gate_json",
        "status",
        "status_detail",
        "worker_card_id",
        "gate_result_json",
        "gate_attempt",
        "gate_pid",
        "gate_pid_starttime",
        "gate_pid_boot_id",
        "running_deadline_ms",
        "created_at_ms",
        "updated_at_ms",
        "finished_at_ms",
        "claim_context_json",
        "context_stale_at_ms",
        "context_closure_truncated",
        "declared_by",
        "decl_ready",
        "decl_released_by_user",
        "context_verify_failures",
        "spawn",
        "child_wave_id",
    ];

    let dir = tempfile::tempdir().expect("temporary database directory");
    let path = dir.path().join("nonterminal-block-at-0072.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("open migration fixture");
    migrator_through_0072()
        .run(&pool)
        .await
        .expect("apply migrations through 0072");
    sqlx::query(
        "INSERT INTO tasks (\
           id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json,\
           priority,gate_json,status,status_detail,worker_card_id,gate_result_json,gate_attempt,\
           gate_pid,gate_pid_starttime,gate_pid_boot_id,running_deadline_ms,created_at_ms,\
           updated_at_ms,finished_at_ms,claim_context_json,context_stale_at_ms,\
           context_closure_truncated,declared_by,origin,decl_ready,decl_released_by_user,\
           context_verify_failures,spawn,child_wave_id\
         ) VALUES (\
           'block-flight','w','block-flight','claude','keep me','{\"source\":\"block\"}',\
           'accept','/tmp','[\"dep\"]',7,'{\"cmd\":\"true\"}','running','working','worker',\
           '{\"ok\":true}',2,101,202,'boot',303,11,12,NULL,'[]',404,1,'spec','block',1,1,5,\
           'child-wave',NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0073 in-flight block task");
    let snapshot_sql = format!(
        "SELECT json_array({}) FROM tasks WHERE id='block-flight'",
        HEAD_TASK_COLUMNS.join(",")
    );
    let before: String = sqlx::query_scalar(&snapshot_sql)
        .fetch_one(&pool)
        .await
        .expect("snapshot complete 0072 task row except origin");
    pool.close().await;

    let repo = SqlxRepo::open(&url)
        .await
        .expect("0073 must allow a normal in-flight block task");
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks') ORDER BY cid")
            .fetch_all(repo.pool())
            .await
            .expect("read task columns after 0073");
    assert_eq!(
        columns, HEAD_TASK_COLUMNS,
        "0073 must remove origin and retain every other task column"
    );
    let after: String = sqlx::query_scalar(&snapshot_sql)
        .fetch_one(repo.pool())
        .await
        .expect("read preserved in-flight block task after 0073");
    assert_eq!(
        after, before,
        "0073 must preserve every persistent value of an in-flight block task"
    );
}

#[tokio::test]
async fn upgrade_0073_accepts_terminal_legacy_rows_and_an_empty_database() {
    let terminal_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open terminal migration fixture");
    migrator_through_0072()
        .run(&terminal_pool)
        .await
        .expect("apply migrations through 0072");
    sqlx::query(
        "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,declared_by,origin,claim_context_json,spawn,decl_ready,decl_released_by_user,context_verify_failures,created_at_ms,updated_at_ms) \
         VALUES ('done','w','done','codex','g','null','[]','done','spec','legacy','[]','in-wave',0,0,0,1,1)",
    )
    .execute(&terminal_pool)
    .await
    .expect("seed pre-0073 terminal legacy task");
    crate::MIGRATOR
        .run(&terminal_pool)
        .await
        .expect("terminal legacy task may upgrade through 0073");
    let terminal_json: String = sqlx::query_scalar(
        "SELECT json_object( \
           'id',id,'wave_id',wave_id,'key',key,'kind',kind,'goal',goal, \
           'context_json',json(context_json),'depends_on_json',json(depends_on_json), \
           'status',status,'declared_by',declared_by, \
           'claim_context_json',json(claim_context_json),'spawn',spawn, \
           'decl_ready',decl_ready,'decl_released_by_user',decl_released_by_user, \
           'context_verify_failures',context_verify_failures, \
           'created_at_ms',created_at_ms,'updated_at_ms',updated_at_ms) \
         FROM tasks WHERE id='done'",
    )
    .fetch_one(&terminal_pool)
    .await
    .expect("0073 must preserve the complete terminal task row");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&terminal_json).unwrap(),
        serde_json::json!({
            "id": "done",
            "wave_id": "w",
            "key": "done",
            "kind": "codex",
            "goal": "g",
            "context_json": null,
            "depends_on_json": [],
            "status": "done",
            "declared_by": "spec",
            "claim_context_json": [],
            "spawn": "in-wave",
            "decl_ready": 0,
            "decl_released_by_user": 0,
            "context_verify_failures": 0,
            "created_at_ms": 1,
            "updated_at_ms": 1
        }),
        "0073 may remove only the origin label, not terminal task data"
    );
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks')")
        .fetch_all(&terminal_pool)
        .await
        .expect("read task columns after 0073");
    assert!(
        !columns.iter().any(|column| column == "origin"),
        "0073 must remove the origin column after preserving terminal rows"
    );

    let empty_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open empty migration fixture");
    migrator_through_0072()
        .run(&empty_pool)
        .await
        .expect("apply migrations through 0072");
    crate::MIGRATOR
        .run(&empty_pool)
        .await
        .expect("empty database may upgrade through 0073");
}
