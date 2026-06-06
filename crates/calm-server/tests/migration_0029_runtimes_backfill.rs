//! PR2a (#488) — migration 0029 runtime backfill.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::collections::HashMap;
use std::str::FromStr;

const MIGRATIONS_UP_TO_0027: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    (
        "0002_plugins",
        include_str!("../migrations/0002_plugins.sql"),
    ),
    (
        "0003_settings",
        include_str!("../migrations/0003_settings.sql"),
    ),
    ("0004_events", include_str!("../migrations/0004_events.sql")),
    (
        "0005_terminals_pid",
        include_str!("../migrations/0005_terminals_pid.sql"),
    ),
    (
        "0006_events_version",
        include_str!("../migrations/0006_events_version.sql"),
    ),
    (
        "0007_events_scope",
        include_str!("../migrations/0007_events_scope.sql"),
    ),
    (
        "0008_cards_role",
        include_str!("../migrations/0008_cards_role.sql"),
    ),
    (
        "0009_coves_kind",
        include_str!("../migrations/0009_coves_kind.sql"),
    ),
    (
        "0010_card_mcp_tokens",
        include_str!("../migrations/0010_card_mcp_tokens.sql"),
    ),
    (
        "0011_terminals_card_id_restrict",
        include_str!("../migrations/0011_terminals_card_id_restrict.sql"),
    ),
    (
        "0012_waves_lifecycle",
        include_str!("../migrations/0012_waves_lifecycle.sql"),
    ),
    (
        "0013_cards_deletable",
        include_str!("../migrations/0013_cards_deletable.sql"),
    ),
    (
        "0014_wave_report_card",
        include_str!("../migrations/0014_wave_report_card.sql"),
    ),
    (
        "0015_cove_folders",
        include_str!("../migrations/0015_cove_folders.sql"),
    ),
    (
        "0016_terminals_theme",
        include_str!("../migrations/0016_terminals_theme.sql"),
    ),
    (
        "0017_terminals_theme_not_null",
        include_str!("../migrations/0017_terminals_theme_not_null.sql"),
    ),
    (
        "0018_wave_cwd_terminal_at",
        include_str!("../migrations/0018_wave_cwd_terminal_at.sql"),
    ),
    (
        "0019_cards_body_crdt",
        include_str!("../migrations/0019_cards_body_crdt.sql"),
    ),
    (
        "0020_terminals_exit_code",
        include_str!("../migrations/0020_terminals_exit_code.sql"),
    ),
    (
        "0021_waves_pinned_at",
        include_str!("../migrations/0021_waves_pinned_at.sql"),
    ),
    (
        "0022_spec_push_queue",
        include_str!("../migrations/0022_spec_push_queue.sql"),
    ),
    (
        "0023_phase3b_proc_handle",
        include_str!("../migrations/0023_phase3b_proc_handle.sql"),
    ),
    (
        "0024_drop_terminals_daemon_handle",
        include_str!("../migrations/0024_drop_terminals_daemon_handle.sql"),
    ),
    (
        "0025_card_codex_threads",
        include_str!("../migrations/0025_card_codex_threads.sql"),
    ),
    (
        "0026_shared_codex_daemon",
        include_str!("../migrations/0026_shared_codex_daemon.sql"),
    ),
    (
        "0027_shared_daemon_env_signature",
        include_str!("../migrations/0027_shared_daemon_env_signature.sql"),
    ),
];

const MIGRATION_0028_SQL: &str = include_str!("../migrations/0028_runtimes.sql");
const MIGRATION_0029_SQL: &str = include_str!("../migrations/0029_runtimes_backfill.sql");

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

async fn pool_staged_at_0027() -> SqlitePool {
    let unique = format!(
        "file:mig0029_{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4().simple()
    );
    let opts = SqliteConnectOptions::from_str(&unique)
        .expect("parse opts")
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.expect("connect");
    for (name, sql) in MIGRATIONS_UP_TO_0027 {
        apply_sql(&pool, name, sql).await;
    }
    pool
}

async fn seed_legacy_live_cards(pool: &SqlitePool) {
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES ('cove-1', 'c', '#000000', 0.0, 'user', 1000, 1000)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO waves
              (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES
              ('wave-1', 'cove-1', 'w', 0.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000),
              ('wave-2', 'cove-1', 'w2', 1.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role, deletable)
           VALUES
              ('card-terminal', 'wave-1', 'terminal', 0.0, '{"terminal_id":"term-terminal"}', 1100, 1100, 'plain', 1),
              ('card-codex', 'wave-1', 'codex', 1.0, '{"terminal_id":"term-codex"}', 1200, 1200, 'plain', 1),
              ('card-claude', 'wave-1', 'claude', 2.0, '{"terminal_id":"term-claude","claude_session_id":"session-claude"}', 1300, 1300, 'worker', 1),
              ('card-shared-thread', 'wave-1', 'codex', 3.0, '{"codex_source":"shared"}', 1400, 1400, 'spec', 0),
              ('card-shared-pending', 'wave-2', 'codex', 4.0, '{"codex_source":"shared"}', 1500, 1500, 'spec', 0),
              ('card-preexisting', 'wave-1', 'terminal', 5.0, '{"terminal_id":"term-preexisting"}', 1600, 1600, 'plain', 1)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO terminals
              (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at, exit_code, signal_killed)
           VALUES
              ('term-terminal', 'card-terminal', 'bash', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1100, NULL, 0),
              ('term-codex', 'card-codex', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1200, NULL, 0),
              ('term-claude', 'card-claude', 'claude', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1300, NULL, 0),
              ('term-preexisting', 'card-preexisting', 'bash', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1600, NULL, 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO card_codex_threads
              (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES
              ('thread-codex', 'card-codex', 'plain', 'wave-1', 1200, 1200),
              ('thread-shared', 'card-shared-thread', 'spec', 'wave-1', 1400, 1400)"#,
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0029_backfills_runtimes_and_is_idempotent() {
    let pool = pool_staged_at_0027().await;
    seed_legacy_live_cards(&pool).await;

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    sqlx::query(
        r#"INSERT INTO runtimes
              (id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms, completed_at_ms)
           VALUES
              ('runtime-preexisting', 'card-preexisting', 'terminal', NULL, 'starting', 'term-preexisting',
               NULL, NULL, NULL, NULL, NULL, NULL, 1600, 1600, NULL)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0029_runtimes_backfill", MIGRATION_0029_SQL).await;

    let rows = sqlx::query(
        r#"SELECT card_id, kind, agent_provider, status, terminal_run_id, thread_id, session_id
           FROM runtimes
           ORDER BY card_id ASC"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let by_card: HashMap<String, sqlx::sqlite::SqliteRow> = rows
        .into_iter()
        .map(|row| (row.try_get::<String, _>("card_id").unwrap(), row))
        .collect();

    let terminal = by_card.get("card-terminal").expect("terminal runtime");
    assert_eq!(terminal.try_get::<String, _>("kind").unwrap(), "terminal");
    assert_eq!(terminal.try_get::<String, _>("status").unwrap(), "starting");
    assert_eq!(
        terminal
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-terminal")
    );

    let codex = by_card.get("card-codex").expect("codex runtime");
    assert_eq!(codex.try_get::<String, _>("kind").unwrap(), "codex");
    assert_eq!(
        codex
            .try_get::<Option<String>, _>("agent_provider")
            .unwrap()
            .as_deref(),
        Some("codex")
    );
    assert_eq!(codex.try_get::<String, _>("status").unwrap(), "running");
    assert_eq!(
        codex
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .as_deref(),
        Some("thread-codex")
    );

    let claude = by_card.get("card-claude").expect("claude runtime");
    assert_eq!(claude.try_get::<String, _>("kind").unwrap(), "claude");
    assert_eq!(claude.try_get::<String, _>("status").unwrap(), "starting");
    assert_eq!(
        claude
            .try_get::<Option<String>, _>("session_id")
            .unwrap()
            .as_deref(),
        Some("session-claude")
    );

    let shared_thread = by_card
        .get("card-shared-thread")
        .expect("shared thread runtime");
    assert_eq!(
        shared_thread.try_get::<String, _>("kind").unwrap(),
        "shared-spec"
    );
    assert_eq!(
        shared_thread.try_get::<String, _>("status").unwrap(),
        "running"
    );
    assert_eq!(
        shared_thread
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .as_deref(),
        Some("thread-shared")
    );
    assert!(
        shared_thread
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .is_none()
    );

    let shared_pending = by_card
        .get("card-shared-pending")
        .expect("shared pending runtime");
    assert_eq!(
        shared_pending.try_get::<String, _>("kind").unwrap(),
        "shared-spec"
    );
    assert_eq!(
        shared_pending.try_get::<String, _>("status").unwrap(),
        "turn_pending"
    );
    assert!(
        shared_pending
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .is_none()
    );

    let preexisting = by_card
        .get("card-preexisting")
        .expect("preexisting runtime");
    assert_eq!(
        preexisting.try_get::<String, _>("status").unwrap(),
        "starting"
    );
    assert_eq!(by_card.len(), 6);

    apply_sql(&pool, "0029_runtimes_backfill", MIGRATION_0029_SQL).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtimes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}
