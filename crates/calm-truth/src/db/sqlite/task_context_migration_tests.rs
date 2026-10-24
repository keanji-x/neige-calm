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
async fn sqlx_repo_open_rejects_nonterminal_legacy_rows_at_0073() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    for status in ["pending", "dispatched", "running", "verifying"] {
        let path = dir.path().join(format!("{status}-at-0072.sqlite"));
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
             VALUES (?1,'w',?1,'codex','g','null','[]',?2,'planner','legacy','[]','in-wave',0,0,0,1,1)",
        )
        .bind(format!("legacy-{status}"))
        .bind(status)
        .execute(&pool)
        .await
        .expect("seed pre-0073 nonterminal legacy task");
        pool.close().await;

        assert!(
            SqlxRepo::open(&url).await.is_err(),
            "the production startup boundary must reject legacy status {status} at 0073"
        );
    }
}

#[tokio::test]
async fn upgrade_from_pre_0068_with_nonterminal_task_aborts_cleanly_at_0072() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    for status in ["pending", "dispatched", "running", "verifying"] {
        let path = dir
            .path()
            .join(format!("pre-0068-{status}-nonterminal.sqlite"));
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
        let task_id = format!("preexisting-{status}");
        sqlx::query(
            "INSERT INTO tasks (id,wave_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms) \
             VALUES (?1,'w',?1,'codex','g','null','[]',?2,1,1)",
        )
        .bind(&task_id)
        .bind(status)
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
            "0073 must fail closed when 0068 attributed preexisting status {status}"
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
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks')")
                .fetch_all(&pool)
                .await
                .expect("read task columns after rejected migration");
        assert!(
            columns.iter().any(|column| column == "origin"),
            "the failed migration must roll back DROP COLUMN origin"
        );
        let origin: String = sqlx::query_scalar("SELECT origin FROM tasks WHERE id=?1")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .expect("read the row preserved by the rejected migration");
        assert_eq!(
            origin, "legacy",
            "0068 must attribute the preexisting {status} row"
        );
        pool.close().await;
    }
}

#[tokio::test]
async fn sqlx_repo_open_accepts_nonterminal_block_row_at_0073_without_data_loss() {
    const PRESERVED_TASK_COLUMNS: &[&str] = &[
        "id",
        "track_id",
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
        "child_track_id",
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
           '{\"ok\":true}',2,101,202,'boot',303,11,12,NULL,'[]',404,1,'planner','block',1,1,5,\
           'child-track',NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0073 in-flight block task");
    // Migration 0081 renames `wave_id` -> `track_id` and `child_wave_id` ->
    // `child_track_id`, so the pre-0081 snapshot needs its own column names.
    let pre_0081_columns: Vec<String> = PRESERVED_TASK_COLUMNS
        .iter()
        .map(|c| match *c {
            "track_id" => "wave_id".to_string(),
            "child_track_id" => "child_wave_id".to_string(),
            other => other.to_string(),
        })
        .collect();
    let snapshot_sql = format!(
        "SELECT json_array({}) FROM tasks WHERE id='block-flight'",
        PRESERVED_TASK_COLUMNS.join(",")
    );
    let before_sql = format!(
        "SELECT json_array({}) FROM tasks WHERE id='block-flight'",
        pre_0081_columns.join(",")
    );
    let before: String = sqlx::query_scalar(&before_sql)
        .fetch_one(&pool)
        .await
        .expect("snapshot complete 0072 task row except origin");
    pool.close().await;

    let repo = SqlxRepo::open(&url)
        .await
        .expect("0073 must allow a normal in-flight block task");
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
         VALUES \
           ('done','w','done','codex','g','null','[]','done','spec','legacy','[]','in-wave',0,0,0,1,1), \
           ('failed','w','failed','codex','g','null','[]','failed','spec','legacy','[]','in-wave',0,0,0,1,1), \
           ('canceled','w','canceled','codex','g','null','[]','canceled','spec','legacy','[]','in-wave',0,0,0,1,1)",
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
           'id',id,'track_id',track_id,'key',key,'kind',kind,'goal',goal, \
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
            "track_id": "w",
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
    let terminal_statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM tasks \
         ORDER BY CASE status WHEN 'done' THEN 1 WHEN 'failed' THEN 2 WHEN 'canceled' THEN 3 END",
    )
    .fetch_all(&terminal_pool)
    .await
    .expect("0073 must preserve every terminal status");
    assert_eq!(terminal_statuses, ["done", "failed", "canceled"]);
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
