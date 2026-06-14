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
          mcp_token_hash TEXT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX ws_token_idx ON worker_sessions(mcp_token_hash)
          WHERE mcp_token_hash IS NOT NULL;
        CREATE TABLE cards (
          id TEXT PRIMARY KEY,
          wave_id TEXT NOT NULL REFERENCES waves(id),
          session_id TEXT NULL REFERENCES worker_sessions(id) ON DELETE SET NULL,
          created_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL
        );
        CREATE TABLE runtimes (
          id TEXT PRIMARY KEY,
          card_id TEXT NOT NULL REFERENCES cards(id),
          status TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL
        );
        CREATE TABLE card_mcp_tokens (
          card_id TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
          hashed_token TEXT NOT NULL
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

async fn card_session_for(pool: &SqlitePool, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn token_for(pool: &SqlitePool, session_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id = ?1")
        .bind(session_id)
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
        r#"INSERT INTO cards (id, wave_id, session_id, created_at, updated_at)
           VALUES
              ('card-a', 'wave-a', NULL, 1, 1),
              ('card-b', 'wave-b', NULL, 1, 1),
              ('card-c', 'wave-c', NULL, 1, 1),
              ('card-d', 'wave-c', NULL, 1, 1)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO worker_sessions
              (id, wave_id, contract, state, mcp_token_hash, created_at_ms, updated_at_ms)
           VALUES
              ('planner-old', 'wave-a', 'planner', 'running', NULL, 100, 200),
              ('planner-newer-created', 'wave-a', 'planner', 'idle', NULL, 300, 300),
              ('planner-newer-id-a', 'wave-a', 'planner', 'turn_pending', NULL, 400, 400),
              ('planner-newer-id-b', 'wave-a', 'planner', 'starting', NULL, 400, 400),
              ('planner-exited', 'wave-a', 'planner', 'exited', NULL, 900, 900),
              ('executor-active', 'wave-a', 'executor', 'running', NULL, 999, 999),
              ('card-a-exited', 'wave-a', 'executor', 'exited', NULL, 700, 700),
              ('planner-b-failed', 'wave-b', 'planner', 'failed', NULL, 1000, 1000),
              ('card-b-exited', 'wave-b', 'executor', 'exited', NULL, 800, 800),
              ('card-c-superseded', 'wave-c', 'executor', 'superseded', NULL, 900, 900),
              ('card-d-active', 'wave-c', 'executor', 'running', NULL, 1000, 1000),
              ('existing-token-session', 'wave-c', 'executor', 'running', 'hash-existing', 1001, 1001)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO runtimes (id, card_id, status, created_at_ms, updated_at_ms)
           VALUES
              ('planner-newer-id-b', 'card-a', 'starting', 400, 400),
              ('card-a-exited', 'card-a', 'exited', 700, 700),
              ('card-a-superseded', 'card-a', 'superseded', 900, 900),
              ('card-b-exited', 'card-b', 'exited', 800, 800),
              ('card-c-superseded', 'card-c', 'superseded', 900, 900),
              ('card-d-active', 'card-d', 'running', 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token)
           VALUES
              ('card-a', 'hash-card-a'),
              ('card-b', 'hash-card-b'),
              ('card-d', 'hash-existing')"#,
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
    assert_eq!(
        card_session_for(&pool, "card-a").await.as_deref(),
        Some("planner-newer-id-b"),
        "active runtime wins over a newer terminal/superseded runtime"
    );
    assert_eq!(
        card_session_for(&pool, "card-b").await.as_deref(),
        Some("card-b-exited"),
        "latest non-superseded runtime backfills the card link when no active runtime exists"
    );
    assert_eq!(
        card_session_for(&pool, "card-c").await,
        None,
        "superseded-only runtimes must not backfill a card link"
    );
    assert_eq!(
        token_for(&pool, "planner-newer-id-b").await.as_deref(),
        Some("hash-card-a"),
        "active worker session receives its card MCP token hash"
    );
    assert_eq!(
        token_for(&pool, "card-b-exited").await,
        None,
        "terminal worker sessions do not receive active-token auth"
    );
    assert_eq!(
        token_for(&pool, "card-d-active").await,
        None,
        "existing duplicate token hash is skipped to preserve ws_token_idx"
    );

    apply_sql(&pool, "0050_root_session_backfill", MIGRATION_0050_SQL).await;
    assert_eq!(
        root_for(&pool, "wave-a").await.as_deref(),
        Some("planner-newer-id-b"),
        "rerunning 0050 must keep the same deterministic root"
    );
    assert_eq!(
        card_session_for(&pool, "card-a").await.as_deref(),
        Some("planner-newer-id-b"),
        "rerunning 0050 must keep the card-session link stable"
    );
    assert_eq!(
        token_for(&pool, "planner-newer-id-b").await.as_deref(),
        Some("hash-card-a"),
        "rerunning 0050 must keep the mirrored token stable"
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
