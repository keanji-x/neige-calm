//! PR9b-iv (#758): migration 0055 retires the runtime mirror table.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATION_0055_SQL: &str = include_str!("../../calm-truth/migrations/0055_drop_runtimes.sql");

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    SqlitePool::connect_with(opts).await.unwrap()
}

async fn apply_sql(pool: &SqlitePool, name: &str, sql: &str) {
    let stripped = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    for raw in stripped.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("migration {name} failed on stmt:\n{trimmed}\nerror: {e}"));
    }
}

async fn stage_pre_0055_schema(pool: &SqlitePool) {
    apply_sql(
        pool,
        "pre-0055",
        r#"
        CREATE TABLE worker_sessions (
          id TEXT PRIMARY KEY,
          card_id TEXT NULL,
          state TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          completed_at_ms INTEGER NULL
        );
        CREATE TABLE "runtimes" (
          id TEXT PRIMARY KEY,
          card_id TEXT NULL,
          status TEXT NOT NULL
        );
        CREATE INDEX runtimes_active_per_card_idx ON "runtimes"(card_id, status);
        CREATE UNIQUE INDEX runtimes_one_active_per_card ON "runtimes"(card_id)
          WHERE status IN ('starting', 'running', 'idle', 'turn_pending');
        CREATE INDEX runtimes_terminal_run_idx ON "runtimes"(id);
        CREATE INDEX runtimes_thread_idx ON "runtimes"(id);
        CREATE INDEX runtimes_session_idx ON "runtimes"(id);
        CREATE INDEX runtimes_recover_scan_idx ON "runtimes"(id);
        "#,
    )
    .await;
}

#[tokio::test]
async fn migration_0055_dedup_resolves_double_active_before_index_create() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;

    sqlx::query(
        r#"INSERT INTO worker_sessions (id, card_id, state, updated_at_ms, completed_at_ms)
           VALUES
             ('old-active', 'card-a', 'running', 100, NULL),
             ('new-active', 'card-a', 'idle', 200, NULL),
             ('terminal-old', 'card-a', 'failed', 300, 300),
             ('other-active', 'card-b', 'turn_pending', 150, NULL),
             ('uncarded-active', NULL, 'running', 400, NULL)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "runtimes" (id, card_id, status)
           VALUES ('old-active', 'card-a', 'running')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let states: Vec<(String, String)> =
        sqlx::query("SELECT id, state FROM worker_sessions WHERE card_id = 'card-a' ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("id"), row.get("state")))
            .collect();
    assert_eq!(
        states,
        vec![
            ("new-active".into(), "idle".into()),
            ("old-active".into(), "superseded".into()),
            ("terminal-old".into(), "failed".into()),
        ]
    );

    let active_card_a: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_sessions
         WHERE card_id = 'card-a'
           AND state IN ('starting', 'running', 'idle', 'turn_pending')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_card_a, 1);

    let ws_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'ws_one_active_per_card'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ws_index, 1);

    let runtime_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'runtimes'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(runtime_table, 0);
}

#[tokio::test]
async fn migration_0055_drops_named_runtime_indexes() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    for name in [
        "runtimes_active_per_card_idx",
        "runtimes_one_active_per_card",
        "runtimes_terminal_run_idx",
        "runtimes_thread_idx",
        "runtimes_session_idx",
        "runtimes_recover_scan_idx",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = ?1",
        )
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "{name} should be gone");
    }
}
