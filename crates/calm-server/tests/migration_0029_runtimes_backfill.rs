//! PR2a (#488) — runtime backfill migration.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

const MIGRATIONS_UP_TO_0027: &[(&str, &str)] = &[
    (
        "0001_init",
        include_str!("../../calm-truth/migrations/0001_init.sql"),
    ),
    (
        "0002_plugins",
        include_str!("../../calm-truth/migrations/0002_plugins.sql"),
    ),
    (
        "0003_settings",
        include_str!("../../calm-truth/migrations/0003_settings.sql"),
    ),
    (
        "0004_events",
        include_str!("../../calm-truth/migrations/0004_events.sql"),
    ),
    (
        "0005_terminals_pid",
        include_str!("../../calm-truth/migrations/0005_terminals_pid.sql"),
    ),
    (
        "0006_events_version",
        include_str!("../../calm-truth/migrations/0006_events_version.sql"),
    ),
    (
        "0007_events_scope",
        include_str!("../../calm-truth/migrations/0007_events_scope.sql"),
    ),
    (
        "0008_cards_role",
        include_str!("../../calm-truth/migrations/0008_cards_role.sql"),
    ),
    (
        "0009_coves_kind",
        include_str!("../../calm-truth/migrations/0009_coves_kind.sql"),
    ),
    (
        "0010_card_mcp_tokens",
        include_str!("../../calm-truth/migrations/0010_card_mcp_tokens.sql"),
    ),
    (
        "0011_terminals_card_id_restrict",
        include_str!("../../calm-truth/migrations/0011_terminals_card_id_restrict.sql"),
    ),
    (
        "0012_waves_lifecycle",
        include_str!("../../calm-truth/migrations/0012_waves_lifecycle.sql"),
    ),
    (
        "0013_cards_deletable",
        include_str!("../../calm-truth/migrations/0013_cards_deletable.sql"),
    ),
    (
        "0014_wave_report_card",
        include_str!("../../calm-truth/migrations/0014_wave_report_card.sql"),
    ),
    (
        "0015_cove_folders",
        include_str!("../../calm-truth/migrations/0015_cove_folders.sql"),
    ),
    (
        "0016_terminals_theme",
        include_str!("../../calm-truth/migrations/0016_terminals_theme.sql"),
    ),
    (
        "0017_terminals_theme_not_null",
        include_str!("../../calm-truth/migrations/0017_terminals_theme_not_null.sql"),
    ),
    (
        "0018_wave_cwd_terminal_at",
        include_str!("../../calm-truth/migrations/0018_wave_cwd_terminal_at.sql"),
    ),
    (
        "0019_cards_body_crdt",
        include_str!("../../calm-truth/migrations/0019_cards_body_crdt.sql"),
    ),
    (
        "0020_terminals_exit_code",
        include_str!("../../calm-truth/migrations/0020_terminals_exit_code.sql"),
    ),
    (
        "0021_waves_pinned_at",
        include_str!("../../calm-truth/migrations/0021_waves_pinned_at.sql"),
    ),
    (
        "0022_spec_push_queue",
        include_str!("../../calm-truth/migrations/0022_spec_push_queue.sql"),
    ),
    (
        "0023_phase3b_proc_handle",
        include_str!("../../calm-truth/migrations/0023_phase3b_proc_handle.sql"),
    ),
    (
        "0024_drop_terminals_daemon_handle",
        include_str!("../../calm-truth/migrations/0024_drop_terminals_daemon_handle.sql"),
    ),
    (
        "0025_card_codex_threads",
        include_str!("../../calm-truth/migrations/0025_card_codex_threads.sql"),
    ),
    (
        "0026_shared_codex_daemon",
        include_str!("../../calm-truth/migrations/0026_shared_codex_daemon.sql"),
    ),
    (
        "0027_shared_daemon_env_signature",
        include_str!("../../calm-truth/migrations/0027_shared_daemon_env_signature.sql"),
    ),
];

const MIGRATION_0028_SQL: &str = include_str!("../../calm-truth/migrations/0028_runtimes.sql");
const MIGRATION_0030_SQL: &str =
    include_str!("../../calm-truth/migrations/0030_runtimes_backfill.sql");
const SQLITE_NOW_MS_SELECT: &str =
    "SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)";

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

async fn sqlite_now_ms(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar(SQLITE_NOW_MS_SELECT)
        .fetch_one(pool)
        .await
        .expect("read sqlite now ms")
}

async fn sqlite_now_ms_away_from_second_boundary(pool: &SqlitePool) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let now_ms = sqlite_now_ms(pool).await;
        let subsecond_ms = now_ms.rem_euclid(1000);
        if (100..=400).contains(&subsecond_ms) {
            return now_ms;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a stable sub-second timestamp window"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn runtime_status_and_completed(pool: &SqlitePool, card_id: &str) -> (String, Option<i64>) {
    let row = sqlx::query("SELECT status, completed_at_ms FROM runtimes WHERE card_id = ?1")
        .bind(card_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (
        row.try_get::<String, _>("status").unwrap(),
        row.try_get::<Option<i64>, _>("completed_at_ms").unwrap(),
    )
}

async fn seed_shared_spec_card_with_terminal(
    pool: &SqlitePool,
    card_id: &str,
    terminal_id: &str,
    payload_terminal_id: Option<&str>,
    thread_id: Option<&str>,
) {
    let wave_id = format!("wave-{card_id}");
    sqlx::query(
        r#"INSERT OR IGNORE INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES ('cove-shared', 'c', '#000000', 0.0, 'user', 1000, 1000)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT OR IGNORE INTO waves
              (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES (?1, 'cove-shared', 'w', 0.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
    )
    .bind(&wave_id)
    .execute(pool)
    .await
    .unwrap();

    let payload = match payload_terminal_id {
        Some(payload_terminal_id) => {
            format!(r#"{{"codex_source":"shared","terminal_id":"{payload_terminal_id}"}}"#)
        }
        None => r#"{"codex_source":"shared"}"#.to_string(),
    };
    sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role, deletable)
           VALUES (?1, ?2, 'codex', 0.0, ?3, 1100, 1100, 'spec', 0)"#,
    )
    .bind(card_id)
    .bind(&wave_id)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO terminals
              (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at, exit_code, signal_killed)
           VALUES (?1, ?2, 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1200, NULL, 0)"#,
    )
    .bind(terminal_id)
    .bind(card_id)
    .execute(pool)
    .await
    .unwrap();

    if let Some(thread_id) = thread_id {
        sqlx::query(
            r#"INSERT INTO card_codex_threads
                  (thread_id, card_id, role, wave_id, created_at, updated_at)
               VALUES (?1, ?2, 'spec', ?3, 1300, 1300)"#,
        )
        .bind(thread_id)
        .bind(card_id)
        .bind(&wave_id)
        .execute(pool)
        .await
        .unwrap();
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
              ('wave-2', 'cove-1', 'w2', 1.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000),
              ('wave-3', 'cove-1', 'w3', 2.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000),
              ('wave-4', 'cove-1', 'w4', 3.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
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
              ('card-shared-pending', 'wave-2', 'codex', 4.0, '{"codex_source":"shared","terminal_id":"term-shared-pending"}', 1500, 1500, 'spec', 0),
              ('card-preexisting', 'wave-1', 'terminal', 5.0, '{"terminal_id":"term-preexisting"}', 1600, 1600, 'plain', 1),
              ('card-codex-threadless', 'wave-1', 'codex', 6.0, '{"terminal_id":"term-codex-threadless"}', 1700, 1700, 'plain', 1),
              ('card-legacy-spec', 'wave-3', 'codex', 7.0, '{"terminal_id":"term-legacy-spec","codex_source":"legacy"}', 1800, 1800, 'spec', 0),
              ('card-codex-shared-worker', 'wave-2', 'codex', 8.0, '{"terminal_id":"term-codex-shared-worker","codex_source":"shared","appserver_sock":"/tmp/codex.sock"}', 1900, 1900, 'worker', 1),
              ('card-codex-shared-plain', 'wave-2', 'codex', 9.0, '{"terminal_id":"term-codex-shared-plain","codex_source":"shared"}', 2000, 2000, 'plain', 1),
              ('card-claude-sessionless', 'wave-2', 'claude', 10.0, '{"terminal_id":"term-claude-sessionless"}', 2100, 2100, 'worker', 1),
              ('card-stale-clean-exit', 'wave-2', 'terminal', 11.0, '{"terminal_id":"term-stale-clean-exit"}', 2200, 2200, 'plain', 1),
              ('card-stale-signal', 'wave-2', 'terminal', 12.0, '{"terminal_id":"term-stale-signal"}', 2300, 2300, 'plain', 1),
              ('card-shared-dead-tui', 'wave-4', 'codex', 13.0, '{"codex_source":"shared","terminal_id":"term-shared-dead-tui"}', 2400, 2400, 'spec', 0)"#,
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
              ('term-preexisting', 'card-preexisting', 'bash', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1600, NULL, 0),
              ('term-codex-threadless', 'card-codex-threadless', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1700, NULL, 0),
              ('term-legacy-spec', 'card-legacy-spec', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1800, NULL, 0),
              ('term-codex-shared-worker', 'card-codex-shared-worker', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1900, NULL, 0),
              ('term-codex-shared-plain', 'card-codex-shared-plain', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2000, NULL, 0),
              ('term-claude-sessionless', 'card-claude-sessionless', 'claude', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2100, NULL, 0),
              ('term-stale-clean-exit', 'card-stale-clean-exit', 'bash', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2200, 0, 0),
              ('term-stale-signal', 'card-stale-signal', 'bash', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2300, NULL, 1),
              ('term-shared-pending', 'card-shared-pending', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2400, NULL, 0),
              ('term-shared-dead-tui', 'card-shared-dead-tui', 'codex', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 2500, 0, 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO card_codex_threads
              (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES
              ('thread-codex', 'card-codex', 'plain', 'wave-1', 1200, 1200),
              ('thread-shared', 'card-shared-thread', 'spec', 'wave-1', 1400, 1400),
              ('thread-legacy-spec', 'card-legacy-spec', 'spec', 'wave-3', 1800, 1800),
              ('t-shared-worker', 'card-codex-shared-worker', 'worker', 'wave-2', 1900, 1900)"#,
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0030_backfills_runtimes_and_is_idempotent() {
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
               NULL, NULL, NULL, NULL, NULL, NULL, 1600, 1600, NULL),
              ('runtime-stale-clean-exit', 'card-stale-clean-exit', 'terminal', NULL, 'starting', 'term-stale-clean-exit',
               NULL, NULL, NULL, NULL, NULL, NULL, 2200, 2200, NULL),
              ('runtime-stale-signal', 'card-stale-signal', 'terminal', NULL, 'starting', 'term-stale-signal',
               NULL, NULL, NULL, NULL, NULL, NULL, 2300, 2300, NULL)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let rows = sqlx::query(
        r#"SELECT card_id, kind, agent_provider, status, terminal_run_id, thread_id, session_id,
                  created_at_ms, updated_at_ms, completed_at_ms
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
    assert_eq!(terminal.try_get::<String, _>("status").unwrap(), "running");
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

    let codex_threadless = by_card
        .get("card-codex-threadless")
        .expect("threadless codex runtime");
    assert_eq!(
        codex_threadless.try_get::<String, _>("kind").unwrap(),
        "codex"
    );
    assert_eq!(
        codex_threadless.try_get::<String, _>("status").unwrap(),
        "turn_pending"
    );
    assert_eq!(
        codex_threadless
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-codex-threadless")
    );
    assert!(
        codex_threadless
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .is_none()
    );
    assert!(
        !by_card.contains_key("card-legacy-spec"),
        "legacy spec codex card must not be backfilled as a plain codex runtime"
    );

    let shared_worker = by_card
        .get("card-codex-shared-worker")
        .expect("shared worker codex runtime");
    assert_eq!(shared_worker.try_get::<String, _>("kind").unwrap(), "codex");
    assert_eq!(
        shared_worker.try_get::<String, _>("status").unwrap(),
        "running"
    );
    assert_eq!(
        shared_worker
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .as_deref(),
        Some("t-shared-worker")
    );
    assert_eq!(
        shared_worker
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-codex-shared-worker")
    );

    let shared_plain = by_card
        .get("card-codex-shared-plain")
        .expect("shared plain codex runtime");
    assert_eq!(shared_plain.try_get::<String, _>("kind").unwrap(), "codex");
    assert_eq!(
        shared_plain.try_get::<String, _>("status").unwrap(),
        "turn_pending"
    );
    assert!(
        shared_plain
            .try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        shared_plain
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-codex-shared-plain")
    );

    let claude = by_card.get("card-claude").expect("claude runtime");
    assert_eq!(claude.try_get::<String, _>("kind").unwrap(), "claude");
    assert_eq!(claude.try_get::<String, _>("status").unwrap(), "running");
    assert_eq!(
        claude
            .try_get::<Option<String>, _>("session_id")
            .unwrap()
            .as_deref(),
        Some("session-claude")
    );

    let claude_sessionless = by_card
        .get("card-claude-sessionless")
        .expect("sessionless claude runtime");
    assert_eq!(
        claude_sessionless.try_get::<String, _>("kind").unwrap(),
        "claude"
    );
    assert_eq!(
        claude_sessionless.try_get::<String, _>("status").unwrap(),
        "running"
    );
    assert_eq!(
        claude_sessionless
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-claude-sessionless")
    );
    assert!(
        claude_sessionless
            .try_get::<Option<String>, _>("session_id")
            .unwrap()
            .is_none()
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
    assert_eq!(
        shared_pending
            .try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-shared-pending")
    );
    assert!(
        !by_card.contains_key("card-shared-dead-tui"),
        "shared spec without a thread must not backfill turn_pending after its TUI exited"
    );

    let preexisting = by_card
        .get("card-preexisting")
        .expect("preexisting runtime");
    assert_eq!(
        preexisting.try_get::<String, _>("status").unwrap(),
        "starting"
    );

    let stale_clean = by_card
        .get("card-stale-clean-exit")
        .expect("stale clean-exit runtime");
    assert_eq!(
        stale_clean.try_get::<String, _>("status").unwrap(),
        "exited"
    );
    let stale_clean_created_at_ms = stale_clean.try_get::<i64, _>("created_at_ms").unwrap();
    let stale_clean_updated_at_ms = stale_clean.try_get::<i64, _>("updated_at_ms").unwrap();
    let stale_clean_completed_at_ms = stale_clean
        .try_get::<Option<i64>, _>("completed_at_ms")
        .unwrap();
    assert!(
        stale_clean_completed_at_ms.is_some(),
        "clean-exit stale runtime should be completed"
    );
    assert!(
        stale_clean_updated_at_ms >= stale_clean_created_at_ms,
        "clean-exit stale runtime update timestamp must not move backwards"
    );
    assert!(
        stale_clean_completed_at_ms.unwrap() >= stale_clean_created_at_ms,
        "clean-exit stale runtime completion timestamp must satisfy runtimes CHECK"
    );

    let stale_signal = by_card
        .get("card-stale-signal")
        .expect("stale signal runtime");
    assert_eq!(
        stale_signal.try_get::<String, _>("status").unwrap(),
        "failed"
    );
    let stale_signal_created_at_ms = stale_signal.try_get::<i64, _>("created_at_ms").unwrap();
    let stale_signal_updated_at_ms = stale_signal.try_get::<i64, _>("updated_at_ms").unwrap();
    let stale_signal_completed_at_ms = stale_signal
        .try_get::<Option<i64>, _>("completed_at_ms")
        .unwrap();
    assert!(
        stale_signal_completed_at_ms.is_some(),
        "signal-killed stale runtime should be completed"
    );
    assert!(
        stale_signal_updated_at_ms >= stale_signal_created_at_ms,
        "signal-killed stale runtime update timestamp must not move backwards"
    );
    assert!(
        stale_signal_completed_at_ms.unwrap() >= stale_signal_created_at_ms,
        "signal-killed stale runtime completion timestamp must satisfy runtimes CHECK"
    );
    assert_eq!(by_card.len(), 12);

    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtimes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 12);
    assert_eq!(
        runtime_status_and_completed(&pool, "card-stale-clean-exit").await,
        ("exited".to_string(), stale_clean_completed_at_ms)
    );
    assert_eq!(
        runtime_status_and_completed(&pool, "card-stale-signal").await,
        ("failed".to_string(), stale_signal_completed_at_ms)
    );
}

#[tokio::test]
async fn migration_0030_completes_stale_runtimes_created_with_subsecond_precision() {
    let pool = pool_staged_at_0027().await;
    seed_legacy_live_cards(&pool).await;

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    let created_at_ms = sqlite_now_ms_away_from_second_boundary(&pool).await;
    assert!(
        (created_at_ms / 1000) * 1000 < created_at_ms,
        "regression seed must include a sub-second component"
    );

    sqlx::query(
        r#"INSERT INTO runtimes
              (id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms, completed_at_ms)
           VALUES
              ('runtime-subsecond-stale-clean-exit', 'card-stale-clean-exit', 'terminal', NULL, 'starting',
               'term-stale-clean-exit', NULL, NULL, NULL, NULL, NULL, NULL, ?1, ?2, NULL)"#,
    )
    .bind(created_at_ms)
    .bind(created_at_ms)
    .execute(&pool)
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(5)).await;
    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let row = sqlx::query(
        r#"SELECT status, created_at_ms, updated_at_ms, completed_at_ms
           FROM runtimes
           WHERE card_id = ?1"#,
    )
    .bind("card-stale-clean-exit")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.try_get::<String, _>("status").unwrap(), "exited");
    assert_eq!(
        row.try_get::<i64, _>("created_at_ms").unwrap(),
        created_at_ms
    );
    assert!(
        row.try_get::<i64, _>("updated_at_ms").unwrap() >= created_at_ms,
        "stale runtime update timestamp must preserve runtimes CHECK"
    );
    assert!(
        row.try_get::<Option<i64>, _>("completed_at_ms")
            .unwrap()
            .expect("completed timestamp")
            >= created_at_ms,
        "stale runtime completion timestamp must preserve runtimes CHECK"
    );
}

#[tokio::test]
async fn migration_0030_shared_spec_pending_keeps_terminal_run_id() {
    let pool = pool_staged_at_0027().await;
    seed_shared_spec_card_with_terminal(
        &pool,
        "card-shared-pending-only",
        "term-shared-pending-only",
        Some("term-shared-pending-only"),
        None,
    )
    .await;

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let row = sqlx::query(
        r#"SELECT kind, status, terminal_run_id, thread_id
           FROM runtimes
           WHERE card_id = ?1"#,
    )
    .bind("card-shared-pending-only")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.try_get::<String, _>("kind").unwrap(), "shared-spec");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "turn_pending");
    assert_eq!(
        row.try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .as_deref(),
        Some("term-shared-pending-only")
    );
    assert!(
        row.try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn migration_0030_shared_spec_running_keeps_terminal_run_id_null() {
    let pool = pool_staged_at_0027().await;
    seed_shared_spec_card_with_terminal(
        &pool,
        "card-shared-running",
        "term-shared-running",
        Some("term-shared-running"),
        Some("thread-shared-running"),
    )
    .await;

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let row = sqlx::query(
        r#"SELECT kind, status, terminal_run_id, thread_id
           FROM runtimes
           WHERE card_id = ?1"#,
    )
    .bind("card-shared-running")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.try_get::<String, _>("kind").unwrap(), "shared-spec");
    assert_eq!(row.try_get::<String, _>("status").unwrap(), "running");
    assert!(
        row.try_get::<Option<String>, _>("terminal_run_id")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("thread_id")
            .unwrap()
            .as_deref(),
        Some("thread-shared-running")
    );
}

#[tokio::test]
async fn migration_0030_shared_spec_orphan_payload_skipped() {
    let pool = pool_staged_at_0027().await;
    seed_shared_spec_card_with_terminal(
        &pool,
        "card-shared-missing-payload-terminal",
        "term-shared-missing-payload-terminal",
        None,
        None,
    )
    .await;
    seed_shared_spec_card_with_terminal(
        &pool,
        "card-shared-wrong-payload-terminal",
        "term-shared-wrong-payload-terminal",
        Some("term-other"),
        None,
    )
    .await;

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM runtimes
           WHERE card_id IN (
             'card-shared-missing-payload-terminal',
             'card-shared-wrong-payload-terminal'
           )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "orphan-payload shared specs must be skipped");
}

#[tokio::test]
async fn migration_0030_skips_orphan_terminal_backfill() {
    let pool = pool_staged_at_0027().await;

    // Minimal cove/wave.
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES ('cove-1', 'c', '#000000', 0.0, 'user', 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO waves
              (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES ('wave-1', 'cove-1', 'w', 0.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Two orphan shapes per kind:
    //   *-missing: payload has no terminal_id at all
    //   *-wrong  : payload.terminal_id points to a different terminal id
    sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role, deletable)
           VALUES
              ('card-term-missing',   'wave-1', 'terminal', 0.0, '{}',                                       1100, 1100, 'plain',  1),
              ('card-term-wrong',     'wave-1', 'terminal', 1.0, '{"terminal_id":"term-other-1"}',           1200, 1200, 'plain',  1),
              ('card-codex-missing',  'wave-1', 'codex',    2.0, '{}',                                       1300, 1300, 'plain',  1),
              ('card-codex-wrong',    'wave-1', 'codex',    3.0, '{"terminal_id":"term-other-2"}',           1400, 1400, 'plain',  1),
              ('card-claude-missing', 'wave-1', 'claude',   4.0, '{}',                                       1500, 1500, 'worker', 1),
              ('card-claude-wrong',   'wave-1', 'claude',   5.0, '{"terminal_id":"term-other-3"}',           1600, 1600, 'worker', 1)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"INSERT INTO terminals
              (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at, exit_code, signal_killed)
           VALUES
              ('term-orphan-1', 'card-term-missing',   'bash',   '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1100, NULL, 0),
              ('term-orphan-2', 'card-term-wrong',     'bash',   '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1200, NULL, 0),
              ('term-orphan-3', 'card-codex-missing',  'codex',  '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1300, NULL, 0),
              ('term-orphan-4', 'card-codex-wrong',    'codex',  '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1400, NULL, 0),
              ('term-orphan-5', 'card-claude-missing', 'claude', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1500, NULL, 0),
              ('term-orphan-6', 'card-claude-wrong',   'claude', '/tmp', '{}', NULL, '216,219,226', '15,20,24', 1600, NULL, 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0028_runtimes", MIGRATION_0028_SQL).await;
    apply_sql(&pool, "0030_runtimes_backfill", MIGRATION_0030_SQL).await;

    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM runtimes
           WHERE card_id IN (
             'card-term-missing','card-term-wrong',
             'card-codex-missing','card-codex-wrong',
             'card-claude-missing','card-claude-wrong'
           )"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count, 0,
        "orphan-terminal cards must not backfill any runtime"
    );
}
