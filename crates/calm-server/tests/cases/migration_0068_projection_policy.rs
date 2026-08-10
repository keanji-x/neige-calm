//! #985 upgrade-day acceptance for migration 0068 and legacy-row adoption.

use calm_server::db::sqlite::project_tasks_tx;
use calm_types::report_blocks::tasks::TaskDeclaration;
use serde_json::json;
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::str::FromStr;

const MIGRATION_0068: &str =
    include_str!("../../../calm-truth/migrations/0068_projection_policy_columns.sql");
const MIGRATION_0069: &str =
    include_str!("../../../calm-truth/migrations/0069_clear_pending_context_stale.sql");
const MIGRATION_0070: &str =
    include_str!("../../../calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql");
const MIGRATION_0071: &str = include_str!("../../../calm-truth/migrations/0071_sub_wave_tree.sql");
const MIGRATION_0072: &str =
    include_str!("../../../calm-truth/migrations/0072_wave_tree_task_budget.sql");
const HEAD_SCHEMA_FIXTURE_MIGRATIONS: &[(&str, &str)] = &[
    ("0068_projection_policy_columns.sql", MIGRATION_0068),
    ("0069_clear_pending_context_stale.sql", MIGRATION_0069),
    (
        "0070_task_context_withdrawal_and_verify.sql",
        MIGRATION_0070,
    ),
    ("0071_sub_wave_tree.sql", MIGRATION_0071),
    ("0072_wave_tree_task_budget.sql", MIGRATION_0072),
];

async fn apply(pool: &SqlitePool, sql: &str) {
    let clean = sql
        .lines()
        .map(|line| line.split("--").next().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    for statement in clean.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(statement).execute(pool).await.unwrap();
    }
}

#[test]
fn head_schema_fixture_lists_every_migration_from_0068_through_head() {
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../calm-truth/migrations");
    let on_disk = fs::read_dir(migrations)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.as_str() >= "0068_")
        .collect::<BTreeSet<_>>();
    let fixture = HEAD_SCHEMA_FIXTURE_MIGRATIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(fixture, on_disk, "head-schema migration fixture drifted");
}

#[tokio::test]
async fn migration_backfills_preexisting_task_and_block_declaration_adopts_it() {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    apply(&pool, r#"
        CREATE TABLE coves(id TEXT PRIMARY KEY, kind TEXT NOT NULL);
        CREATE TABLE waves(id TEXT PRIMARY KEY, cove_id TEXT NOT NULL, require_task_gates INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE tasks(
          id TEXT PRIMARY KEY, wave_id TEXT NOT NULL, key TEXT NOT NULL, kind TEXT NOT NULL,
          goal TEXT NOT NULL, context_json TEXT NOT NULL, acceptance_criteria TEXT NULL,
          cwd TEXT NULL, depends_on_json TEXT NOT NULL, priority INTEGER NOT NULL,
          gate_json TEXT NULL, status TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL, claim_context_json TEXT NULL,
          context_stale_at_ms INTEGER NULL,
          context_closure_truncated INTEGER NOT NULL DEFAULT 0,
          UNIQUE(wave_id,key)
        );
        INSERT INTO coves VALUES('c1','user');
        INSERT INTO waves VALUES('w1','c1',0);
        INSERT INTO tasks(
          id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,
          depends_on_json,priority,gate_json,status,created_at_ms,updated_at_ms,
          claim_context_json,context_stale_at_ms,context_closure_truncated
        ) VALUES(
          'old','w1','adopt','codex','old goal','{}',NULL,NULL,'[]',0,NULL,
          'dispatched',1,1,'[]',NULL,0
        );
    "#).await;
    apply(&pool, HEAD_SCHEMA_FIXTURE_MIGRATIONS[0].1).await;

    let attribution: (String, String) =
        sqlx::query_as("SELECT declared_by,origin FROM tasks WHERE id='old'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attribution, ("spec".into(), "legacy".into()));

    // Production projection runs against the schema at head. Keep the 0068
    // backfill assertion above isolated, then bring this minimal fixture up to
    // the first schema version required by the production write path.
    for (_, migration) in &HEAD_SCHEMA_FIXTURE_MIGRATIONS[1..] {
        apply(&pool, migration).await;
    }

    let declaration = TaskDeclaration {
        block_index: Some(0),
        block_id: "b_adopt".into(),
        key: "adopt".into(),
        kind: "codex".into(),
        goal: "old goal".into(),
        acceptance: None,
        gate: None,
        no_gate_reason: None,
        depends_on: vec![],
        context: json!({}),
        cwd: None,
        priority: 0,
        refs: vec![],
        declared_by: "spec".into(),
        released_by_user: false,
        spawn: "in-wave".into(),
        tombstoned_by: None,
        ready: true,
        tombstone: false,
    };
    let mut tx = pool.begin().await.unwrap();
    let outcome = project_tasks_tx(&mut tx, "w1", &[declaration], &[vec![]])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        outcome.changed_keys,
        ["adopt"],
        "adoption must not silently affect zero rows"
    );
    let row: (String, String, String) =
        sqlx::query_as("SELECT id,declared_by,origin FROM tasks WHERE key='adopt'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("old".into(), "spec".into(), "block".into()));
}
