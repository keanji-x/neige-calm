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
        CREATE TABLE waves (
          id TEXT PRIMARY KEY,
          root_session_id TEXT NULL
        );
        CREATE TABLE cards (
          id TEXT PRIMARY KEY,
          wave_id TEXT NOT NULL,
          session_id TEXT NULL
        );
        CREATE TABLE terminals (
          id TEXT PRIMARY KEY
        );
        CREATE TABLE operations (
          id TEXT PRIMARY KEY
        );
        CREATE TABLE worker_sessions (
          id TEXT PRIMARY KEY,
          wave_id TEXT NOT NULL,
          provider TEXT NOT NULL,
          mode TEXT NOT NULL,
          contract TEXT NOT NULL,
          parent_session_id TEXT NULL,
          requester_session_id TEXT NULL,
          state TEXT NOT NULL,
          mcp_token_hash TEXT NULL,
          thread_id TEXT NULL,
          agent_session_id TEXT NULL,
          active_turn_id TEXT NULL,
          terminal_run_id TEXT NULL,
          handle_state_json TEXT NULL,
          liveness TEXT NOT NULL DEFAULT 'unknown',
          liveness_probed_at_ms INTEGER NULL,
          exit_code INTEGER NULL,
          exit_interpretation TEXT NULL,
          spawn_op_id TEXT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          completed_at_ms INTEGER NULL,
          last_activity_ms INTEGER NULL,
          last_thread_status TEXT NULL,
          card_id TEXT NULL
        );
        CREATE TABLE "runtimes" (
          id TEXT PRIMARY KEY,
          card_id TEXT NULL,
          kind TEXT NOT NULL,
          agent_provider TEXT NULL,
          status TEXT NOT NULL,
          terminal_run_id TEXT NULL,
          thread_id TEXT NULL,
          session_id TEXT NULL,
          active_turn_id TEXT NULL,
          handle_state_json TEXT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          completed_at_ms INTEGER NULL
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

async fn seed_card(pool: &SqlitePool, wave_id: &str, card_id: &str) {
    sqlx::query("INSERT OR IGNORE INTO waves (id, root_session_id) VALUES (?1, NULL)")
        .bind(wave_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO cards (id, wave_id, session_id) VALUES (?1, ?2, NULL)")
        .bind(card_id)
        .bind(wave_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_worker_session(
    pool: &SqlitePool,
    id: &str,
    wave_id: &str,
    card_id: Option<&str>,
    state: &str,
    created_at_ms: i64,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
) {
    sqlx::query(
        r#"INSERT INTO worker_sessions (
             id, wave_id, provider, mode, contract, state, liveness,
             created_at_ms, updated_at_ms, completed_at_ms, card_id
           )
           VALUES (
             ?1, ?2, 'codex', 'resumable', 'planner', ?3, 'unknown',
             ?4, ?5, ?6, ?7
           )"#,
    )
    .bind(id)
    .bind(wave_id)
    .bind(state)
    .bind(created_at_ms)
    .bind(updated_at_ms)
    .bind(completed_at_ms)
    .bind(card_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_runtime(
    pool: &SqlitePool,
    id: &str,
    card_id: &str,
    kind: &str,
    agent_provider: Option<&str>,
    status: &str,
    created_at_ms: i64,
    updated_at_ms: i64,
) {
    sqlx::query(
        r#"INSERT INTO "runtimes" (
             id, card_id, kind, agent_provider, status, terminal_run_id,
             thread_id, session_id, active_turn_id, handle_state_json,
             created_at_ms, updated_at_ms, completed_at_ms
           )
           VALUES (
             ?1, ?2, ?3, ?4, ?5, NULL,
             'thread-0055', 'agent-session-0055', 'turn-0055',
             '{"mode":"harness"}', ?6, ?7, NULL
           )"#,
    )
    .bind(id)
    .bind(card_id)
    .bind(kind)
    .bind(agent_provider)
    .bind(status)
    .bind(created_at_ms)
    .bind(updated_at_ms)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0055_backfills_runtimes_without_ws_mirror() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-a", "card-a").await;
    insert_runtime(
        &pool,
        "runtime-only",
        "card-a",
        "shared-spec",
        Some("codex"),
        "idle",
        100,
        200,
    )
    .await;
    insert_runtime(
        &pool,
        "orphan-runtime",
        "deleted-card",
        "codex",
        Some("codex"),
        "running",
        110,
        210,
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let row = sqlx::query(
        r#"SELECT id, wave_id, provider, mode, contract, state,
                  thread_id, agent_session_id, active_turn_id, handle_state_json,
                  liveness, created_at_ms, updated_at_ms, completed_at_ms, card_id
             FROM worker_sessions
            WHERE id = 'runtime-only'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("wave_id"), "wave-a");
    assert_eq!(row.get::<String, _>("provider"), "codex");
    assert_eq!(row.get::<String, _>("mode"), "resumable");
    assert_eq!(row.get::<String, _>("contract"), "planner");
    assert_eq!(row.get::<String, _>("state"), "idle");
    assert_eq!(row.get::<String, _>("thread_id"), "thread-0055");
    assert_eq!(row.get::<String, _>("agent_session_id"), "agent-session-0055");
    assert_eq!(row.get::<String, _>("active_turn_id"), "turn-0055");
    assert_eq!(row.get::<String, _>("handle_state_json"), r#"{"mode":"harness"}"#);
    assert_eq!(row.get::<String, _>("liveness"), "unknown");
    assert_eq!(row.get::<i64, _>("created_at_ms"), 100);
    assert_eq!(row.get::<i64, _>("updated_at_ms"), 200);
    assert_eq!(row.get::<Option<i64>, _>("completed_at_ms"), None);
    assert_eq!(row.get::<String, _>("card_id"), "card-a");

    let card_session: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = 'card-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(card_session.as_deref(), Some("runtime-only"));

    let orphan_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE id = 'orphan-runtime'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(orphan_count, 0);
}

#[tokio::test]
async fn migration_0055_dedup_resolves_double_active_before_index_create() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-a", "card-a").await;
    seed_card(&pool, "wave-b", "card-b").await;

    insert_worker_session(&pool, "old-active", "wave-a", Some("card-a"), "running", 90, 100, None)
        .await;
    insert_worker_session(&pool, "new-active", "wave-a", Some("card-a"), "idle", 190, 200, None)
        .await;
    insert_worker_session(
        &pool,
        "terminal-old",
        "wave-a",
        Some("card-a"),
        "failed",
        290,
        300,
        Some(300),
    )
    .await;
    insert_worker_session(
        &pool,
        "other-active",
        "wave-b",
        Some("card-b"),
        "turn_pending",
        140,
        150,
        None,
    )
    .await;
    insert_worker_session(
        &pool,
        "uncarded-active",
        "wave-a",
        None,
        "running",
        390,
        400,
        None,
    )
    .await;
    insert_runtime(
        &pool,
        "old-active",
        "card-a",
        "shared-spec",
        Some("codex"),
        "running",
        90,
        100,
    )
    .await;

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
