//! PR9b-iv (#758): migration 0055 retires the runtime mirror table.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATION_0055_SQL: &str =
    include_str!("../../../calm-truth/migrations/0055_drop_runtimes.sql");

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
        CREATE TABLE card_mcp_tokens (
          card_id TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
          hashed_token TEXT NOT NULL,
          created_at INTEGER NOT NULL
        );
        CREATE INDEX idx_card_mcp_tokens_hashed
          ON card_mcp_tokens(hashed_token);
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
        CREATE UNIQUE INDEX ws_token_idx ON worker_sessions(mcp_token_hash)
          WHERE mcp_token_hash IS NOT NULL;
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

async fn seed_card_mcp_token(pool: &SqlitePool, card_id: &str, hashed_token: &str) {
    sqlx::query(
        "INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at) VALUES (?1, ?2, 1234)",
    )
    .bind(card_id)
    .bind(hashed_token)
    .execute(pool)
    .await
    .unwrap();
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

async fn set_card_session(pool: &SqlitePool, card_id: &str, session_id: &str) {
    sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
        .bind(session_id)
        .bind(card_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn set_wave_root(pool: &SqlitePool, wave_id: &str, session_id: &str) {
    sqlx::query("UPDATE waves SET root_session_id = ?1 WHERE id = ?2")
        .bind(session_id)
        .bind(wave_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn worker_session_state(pool: &SqlitePool, session_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

struct WorkerSessionSeed<'a> {
    id: &'a str,
    wave_id: &'a str,
    card_id: Option<&'a str>,
    state: &'a str,
    created_at_ms: i64,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
}

async fn insert_worker_session(pool: &SqlitePool, seed: WorkerSessionSeed<'_>) {
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
    .bind(seed.id)
    .bind(seed.wave_id)
    .bind(seed.state)
    .bind(seed.created_at_ms)
    .bind(seed.updated_at_ms)
    .bind(seed.completed_at_ms)
    .bind(seed.card_id)
    .execute(pool)
    .await
    .unwrap();
}

struct RuntimeSeed<'a> {
    id: &'a str,
    card_id: &'a str,
    kind: &'a str,
    agent_provider: Option<&'a str>,
    status: &'a str,
    created_at_ms: i64,
    updated_at_ms: i64,
}

async fn insert_runtime(pool: &SqlitePool, seed: RuntimeSeed<'_>) {
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
    .bind(seed.id)
    .bind(seed.card_id)
    .bind(seed.kind)
    .bind(seed.agent_provider)
    .bind(seed.status)
    .bind(seed.created_at_ms)
    .bind(seed.updated_at_ms)
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
        RuntimeSeed {
            id: "runtime-only",
            card_id: "card-a",
            kind: "shared-spec",
            agent_provider: Some("codex"),
            status: "idle",
            created_at_ms: 100,
            updated_at_ms: 200,
        },
    )
    .await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "orphan-runtime",
            card_id: "deleted-card",
            kind: "codex",
            agent_provider: Some("codex"),
            status: "running",
            created_at_ms: 110,
            updated_at_ms: 210,
        },
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
    assert_eq!(
        row.get::<String, _>("agent_session_id"),
        "agent-session-0055"
    );
    assert_eq!(row.get::<String, _>("active_turn_id"), "turn-0055");
    assert_eq!(
        row.get::<String, _>("handle_state_json"),
        r#"{"mode":"harness"}"#
    );
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
async fn migration_0055_repoints_waves_root_session_for_bridged_planner() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-root", "card-root").await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "bridged-planner",
            card_id: "card-root",
            kind: "shared-spec",
            agent_provider: Some("codex"),
            status: "idle",
            created_at_ms: 100,
            updated_at_ms: 200,
        },
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let root_session_id: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = 'wave-root'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(root_session_id.as_deref(), Some("bridged-planner"));
}

#[tokio::test]
async fn migration_0055_mirrors_mcp_token_for_bridged_session() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-token", "card-token").await;
    seed_card_mcp_token(&pool, "card-token", "hash-token").await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "bridged-token",
            card_id: "card-token",
            kind: "codex",
            agent_provider: Some("codex"),
            status: "running",
            created_at_ms: 100,
            updated_at_ms: 200,
        },
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let mcp_token_hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id = 'bridged-token'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mcp_token_hash.as_deref(), Some("hash-token"));
}

#[tokio::test]
async fn migration_0055_root_session_repoint_skips_already_set_waves() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-keep", "card-keep").await;
    seed_card(&pool, "wave-keep", "card-new").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "valid-root",
            wave_id: "wave-keep",
            card_id: Some("card-keep"),
            state: "idle",
            created_at_ms: 100,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    sqlx::query("UPDATE waves SET root_session_id = 'valid-root' WHERE id = 'wave-keep'")
        .execute(&pool)
        .await
        .unwrap();
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "newer-bridged-planner",
            card_id: "card-new",
            kind: "shared-spec",
            agent_provider: Some("codex"),
            status: "running",
            created_at_ms: 200,
            updated_at_ms: 300,
        },
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let root_session_id: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = 'wave-keep'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(root_session_id.as_deref(), Some("valid-root"));
}

#[tokio::test]
async fn migration_0055_token_mirror_skips_duplicate_hashed_tokens() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-dupe", "card-dupe-a").await;
    seed_card(&pool, "wave-dupe", "card-dupe-b").await;
    seed_card_mcp_token(&pool, "card-dupe-a", "hash-dupe").await;
    seed_card_mcp_token(&pool, "card-dupe-b", "hash-dupe").await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "bridged-dupe-token",
            card_id: "card-dupe-a",
            kind: "codex",
            agent_provider: Some("codex"),
            status: "idle",
            created_at_ms: 100,
            updated_at_ms: 200,
        },
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let mcp_token_hash: Option<String> = sqlx::query_scalar(
        "SELECT mcp_token_hash FROM worker_sessions WHERE id = 'bridged-dupe-token'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mcp_token_hash, None);
}

#[tokio::test]
async fn migration_0055_dedup_resolves_double_active_before_index_create() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-a", "card-a").await;
    seed_card(&pool, "wave-b", "card-b").await;

    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "old-active",
            wave_id: "wave-a",
            card_id: Some("card-a"),
            state: "running",
            created_at_ms: 90,
            updated_at_ms: 200,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "new-active",
            wave_id: "wave-a",
            card_id: Some("card-a"),
            state: "idle",
            created_at_ms: 190,
            updated_at_ms: 200,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "terminal-old",
            wave_id: "wave-a",
            card_id: Some("card-a"),
            state: "failed",
            created_at_ms: 290,
            updated_at_ms: 300,
            completed_at_ms: Some(300),
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "other-active",
            wave_id: "wave-b",
            card_id: Some("card-b"),
            state: "turn_pending",
            created_at_ms: 140,
            updated_at_ms: 150,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "uncarded-active",
            wave_id: "wave-a",
            card_id: None,
            state: "running",
            created_at_ms: 390,
            updated_at_ms: 400,
            completed_at_ms: None,
        },
    )
    .await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "old-active",
            card_id: "card-a",
            kind: "shared-spec",
            agent_provider: Some("codex"),
            status: "running",
            created_at_ms: 90,
            updated_at_ms: 200,
        },
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
async fn migration_0055_repoints_cards_session_id_from_superseded() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-card-stale", "card-stale").await;

    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-card-old",
            wave_id: "wave-card-stale",
            card_id: Some("card-stale"),
            state: "running",
            created_at_ms: 100,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-card-new",
            wave_id: "wave-card-stale",
            card_id: Some("card-stale"),
            state: "idle",
            created_at_ms: 200,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    set_card_session(&pool, "card-stale", "ws-card-old").await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "ws-card-old",
            card_id: "card-stale",
            kind: "codex",
            agent_provider: Some("codex"),
            status: "running",
            created_at_ms: 100,
            updated_at_ms: 100,
        },
    )
    .await;
    insert_runtime(
        &pool,
        RuntimeSeed {
            id: "ws-card-new",
            card_id: "card-stale",
            kind: "codex",
            agent_provider: Some("codex"),
            status: "exited",
            created_at_ms: 200,
            updated_at_ms: 100,
        },
    )
    .await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let card_session: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = 'card-stale'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(card_session.as_deref(), Some("ws-card-new"));
    assert_eq!(
        worker_session_state(&pool, "ws-card-old").await,
        "superseded"
    );
    assert_eq!(worker_session_state(&pool, "ws-card-new").await, "idle");
}

#[tokio::test]
async fn migration_0055_repoints_waves_root_from_superseded() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-root-stale", "card-root-stale").await;

    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-root-old",
            wave_id: "wave-root-stale",
            card_id: Some("card-root-stale"),
            state: "running",
            created_at_ms: 100,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-root-new",
            wave_id: "wave-root-stale",
            card_id: Some("card-root-stale"),
            state: "turn_pending",
            created_at_ms: 200,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    set_card_session(&pool, "card-root-stale", "ws-root-old").await;
    set_wave_root(&pool, "wave-root-stale", "ws-root-old").await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let root_session_id: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = 'wave-root-stale'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(root_session_id.as_deref(), Some("ws-root-new"));
    assert_eq!(
        worker_session_state(&pool, "ws-root-old").await,
        "superseded"
    );
    assert_eq!(
        worker_session_state(&pool, "ws-root-new").await,
        "turn_pending"
    );
}

#[tokio::test]
async fn migration_0055_cards_session_id_unchanged_for_terminal_pointer() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-terminal-card", "card-terminal").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-card-terminal",
            wave_id: "wave-terminal-card",
            card_id: Some("card-terminal"),
            state: "exited",
            created_at_ms: 100,
            updated_at_ms: 200,
            completed_at_ms: Some(200),
        },
    )
    .await;
    set_card_session(&pool, "card-terminal", "ws-card-terminal").await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let card_session: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = 'card-terminal'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(card_session.as_deref(), Some("ws-card-terminal"));
    assert_eq!(
        worker_session_state(&pool, "ws-card-terminal").await,
        "exited"
    );
}

#[tokio::test]
async fn migration_0055_waves_root_unchanged_for_active_root() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;
    seed_card(&pool, "wave-active-root", "card-active-root").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-active-root",
            wave_id: "wave-active-root",
            card_id: Some("card-active-root"),
            state: "idle",
            created_at_ms: 100,
            updated_at_ms: 200,
            completed_at_ms: None,
        },
    )
    .await;
    set_wave_root(&pool, "wave-active-root", "ws-active-root").await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let root_session_id: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM waves WHERE id = 'wave-active-root'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(root_session_id.as_deref(), Some("ws-active-root"));
}

#[tokio::test]
async fn migration_0055_full_systemic_audit() {
    let pool = fresh_pool().await;
    stage_pre_0055_schema(&pool).await;

    seed_card(&pool, "wave-clean-card", "card-clean").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-clean-card",
            wave_id: "wave-clean-card",
            card_id: Some("card-clean"),
            state: "idle",
            created_at_ms: 100,
            updated_at_ms: 100,
            completed_at_ms: None,
        },
    )
    .await;
    set_card_session(&pool, "card-clean", "ws-clean-card").await;

    seed_card(&pool, "wave-null-card", "card-null").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-null-card",
            wave_id: "wave-null-card",
            card_id: Some("card-null"),
            state: "running",
            created_at_ms: 110,
            updated_at_ms: 110,
            completed_at_ms: None,
        },
    )
    .await;

    seed_card(&pool, "wave-superseded-card", "card-superseded").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-superseded-card-old",
            wave_id: "wave-superseded-card",
            card_id: Some("card-superseded"),
            state: "running",
            created_at_ms: 120,
            updated_at_ms: 120,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-superseded-card-new",
            wave_id: "wave-superseded-card",
            card_id: Some("card-superseded"),
            state: "turn_pending",
            created_at_ms: 130,
            updated_at_ms: 120,
            completed_at_ms: None,
        },
    )
    .await;
    set_card_session(&pool, "card-superseded", "ws-superseded-card-old").await;

    seed_card(&pool, "wave-terminal-pointer", "card-terminal-pointer").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-terminal-pointer",
            wave_id: "wave-terminal-pointer",
            card_id: Some("card-terminal-pointer"),
            state: "failed",
            created_at_ms: 140,
            updated_at_ms: 140,
            completed_at_ms: Some(140),
        },
    )
    .await;
    set_card_session(&pool, "card-terminal-pointer", "ws-terminal-pointer").await;

    seed_card(&pool, "wave-null-root-mix", "card-null-root-mix").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-null-root-mix",
            wave_id: "wave-null-root-mix",
            card_id: Some("card-null-root-mix"),
            state: "idle",
            created_at_ms: 150,
            updated_at_ms: 150,
            completed_at_ms: None,
        },
    )
    .await;

    seed_card(
        &pool,
        "wave-superseded-root-mix",
        "card-superseded-root-mix",
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-superseded-root-old",
            wave_id: "wave-superseded-root-mix",
            card_id: Some("card-superseded-root-mix"),
            state: "running",
            created_at_ms: 160,
            updated_at_ms: 160,
            completed_at_ms: None,
        },
    )
    .await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-superseded-root-new",
            wave_id: "wave-superseded-root-mix",
            card_id: Some("card-superseded-root-mix"),
            state: "idle",
            created_at_ms: 170,
            updated_at_ms: 160,
            completed_at_ms: None,
        },
    )
    .await;
    set_wave_root(&pool, "wave-superseded-root-mix", "ws-superseded-root-old").await;

    seed_card(&pool, "wave-active-root-mix", "card-active-root-mix").await;
    insert_worker_session(
        &pool,
        WorkerSessionSeed {
            id: "ws-active-root-mix",
            wave_id: "wave-active-root-mix",
            card_id: Some("card-active-root-mix"),
            state: "turn_pending",
            created_at_ms: 180,
            updated_at_ms: 180,
            completed_at_ms: None,
        },
    )
    .await;
    set_wave_root(&pool, "wave-active-root-mix", "ws-active-root-mix").await;

    apply_sql(&pool, "0055_drop_runtimes", MIGRATION_0055_SQL).await;

    let card_sessions: Vec<(String, Option<String>)> =
        sqlx::query("SELECT id, session_id FROM cards ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("id"), row.get("session_id")))
            .collect();
    assert!(card_sessions.contains(&("card-clean".into(), Some("ws-clean-card".into()))));
    assert!(card_sessions.contains(&("card-null".into(), Some("ws-null-card".into()))));
    assert!(card_sessions.contains(&(
        "card-superseded".into(),
        Some("ws-superseded-card-new".into())
    )));
    assert!(card_sessions.contains(&(
        "card-terminal-pointer".into(),
        Some("ws-terminal-pointer".into())
    )));

    let wave_roots: Vec<(String, Option<String>)> =
        sqlx::query("SELECT id, root_session_id FROM waves ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("id"), row.get("root_session_id")))
            .collect();
    assert!(wave_roots.contains(&("wave-null-root-mix".into(), Some("ws-null-root-mix".into()))));
    assert!(wave_roots.contains(&(
        "wave-superseded-root-mix".into(),
        Some("ws-superseded-root-new".into())
    )));
    assert!(wave_roots.contains(&(
        "wave-active-root-mix".into(),
        Some("ws-active-root-mix".into())
    )));
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
