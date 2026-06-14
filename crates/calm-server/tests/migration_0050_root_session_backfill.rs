//! #679 PR7b-i Unit 1: migration 0050 deterministically backfills wave roots.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATION_0050_SQL: &str =
    include_str!("../../calm-truth/migrations/0050_root_session_backfill.sql");

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

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    SqlitePool::connect_with(opts).await.unwrap()
}

async fn stage_pre_0050_schema(pool: &SqlitePool) {
    apply_sql(
        pool,
        "pre-0050",
        r#"
        CREATE TABLE waves (
          id TEXT PRIMARY KEY
        );
        CREATE TABLE worker_sessions (
          id TEXT PRIMARY KEY,
          wave_id TEXT NOT NULL REFERENCES waves(id),
          contract TEXT NOT NULL,
          state TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL
        );
        ALTER TABLE waves ADD COLUMN root_session_id TEXT NULL
          REFERENCES worker_sessions(id);
        "#,
    )
    .await;
}

async fn root_for(pool: &SqlitePool, wave_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_0050_backfills_latest_active_planner_per_wave() {
    let pool = fresh_pool().await;
    stage_pre_0050_schema(&pool).await;

    sqlx::query("INSERT INTO waves (id) VALUES ('wave-a'), ('wave-b'), ('wave-c')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO worker_sessions
              (id, wave_id, contract, state, created_at_ms, updated_at_ms)
           VALUES
              ('planner-old', 'wave-a', 'planner', 'running', 100, 200),
              ('planner-newer-created', 'wave-a', 'planner', 'idle', 300, 300),
              ('planner-newer-id-a', 'wave-a', 'planner', 'turn_pending', 400, 400),
              ('planner-newer-id-b', 'wave-a', 'planner', 'starting', 400, 400),
              ('planner-exited', 'wave-a', 'planner', 'exited', 900, 900),
              ('executor-active', 'wave-a', 'executor', 'running', 999, 999),
              ('planner-b-failed', 'wave-b', 'planner', 'failed', 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0050_root_session_backfill", MIGRATION_0050_SQL).await;

    assert_eq!(
        root_for(&pool, "wave-a").await.as_deref(),
        Some("planner-newer-id-b")
    );
    assert_eq!(root_for(&pool, "wave-b").await, None);
    assert_eq!(root_for(&pool, "wave-c").await, None);

    apply_sql(&pool, "0050_root_session_backfill", MIGRATION_0050_SQL).await;
    assert_eq!(
        root_for(&pool, "wave-a").await.as_deref(),
        Some("planner-newer-id-b"),
        "rerunning 0050 must keep the same deterministic root"
    );

    let fk = sqlx::query(
        r#"SELECT "table", "from", "to"
           FROM pragma_foreign_key_list('waves')
           WHERE "from" = 'root_session_id'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fk.get::<String, _>("table"), "worker_sessions");
    assert_eq!(fk.get::<String, _>("to"), "id");
}
