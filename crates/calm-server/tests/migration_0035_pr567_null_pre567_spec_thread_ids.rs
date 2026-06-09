//! #570 P2-C — migration 0035 nulls reusable pre-PR #567 spec thread ids.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATIONS_UP_TO_0034: &[(&str, &str)] = &[
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
    (
        "0028_runtimes",
        include_str!("../migrations/0028_runtimes.sql"),
    ),
    (
        "0029_operations",
        include_str!("../migrations/0029_operations.sql"),
    ),
    (
        "0030_runtimes_backfill",
        include_str!("../migrations/0030_runtimes_backfill.sql"),
    ),
    (
        "0031_harness_items",
        include_str!("../migrations/0031_harness_items.sql"),
    ),
    (
        "0032_harness_items_fk",
        include_str!("../migrations/0032_harness_items_fk.sql"),
    ),
    (
        "0033_drop_spec_push_queue",
        include_str!("../migrations/0033_drop_spec_push_queue.sql"),
    ),
    (
        "0034_drop_card_codex_threads",
        include_str!("../migrations/0034_drop_card_codex_threads.sql"),
    ),
];

const MIGRATION_0035_SQL: &str =
    include_str!("../migrations/0035_pr567_null_pre567_spec_thread_ids.sql");

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

async fn seed_wave(pool: &SqlitePool, wave_id: &str) {
    sqlx::query(
        r#"INSERT INTO waves
              (id, cove_id, title, sort, archived_at, created_at, updated_at)
           VALUES (?1, 'cove-0035', 'w', 0.0, NULL, 1000, 1000)"#,
    )
    .bind(wave_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_card(pool: &SqlitePool, card_id: &str, wave_id: &str, role: Option<&str>) {
    match role {
        Some(role) => {
            sqlx::query(
                r#"INSERT INTO cards
                      (id, wave_id, kind, sort, payload, created_at, updated_at, role)
                   VALUES (?1, ?2, 'codex', 0.0, '{}', 1000, 1000, ?3)"#,
            )
            .bind(card_id)
            .bind(wave_id)
            .bind(role)
            .execute(pool)
            .await
            .unwrap();
        }
        None => {
            sqlx::query(
                r#"INSERT INTO cards
                      (id, wave_id, kind, sort, payload, created_at, updated_at)
                   VALUES (?1, ?2, 'codex', 0.0, '{}', 1000, 1000)"#,
            )
            .bind(card_id)
            .bind(wave_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }
}

async fn seed_runtime(
    pool: &SqlitePool,
    runtime_id: &str,
    card_id: &str,
    thread_id: &str,
    handle_state_json: Option<&str>,
) {
    sqlx::query(
        r#"INSERT INTO runtimes (
               id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
               completed_at_ms
           )
           VALUES (?1, ?2, 'shared-spec', 'codex', 'idle', NULL,
                   ?3, NULL, NULL, ?4, NULL, NULL, 1100, 1100, NULL)"#,
    )
    .bind(runtime_id)
    .bind(card_id)
    .bind(thread_id)
    .bind(handle_state_json)
    .execute(pool)
    .await
    .unwrap();
}

async fn thread_id(pool: &SqlitePool, runtime_id: &str) -> Option<String> {
    let row = sqlx::query("SELECT thread_id FROM runtimes WHERE id = ?1")
        .bind(runtime_id)
        .fetch_one(pool)
        .await
        .unwrap();
    row.try_get::<Option<String>, _>("thread_id").unwrap()
}

async fn snapshot_last_thread_id(pool: &SqlitePool, runtime_id: &str) -> Option<String> {
    let row = sqlx::query(
        "SELECT json_extract(handle_state_json, '$.last_thread_id') AS last_thread_id FROM runtimes WHERE id = ?1",
    )
    .bind(runtime_id)
    .fetch_one(pool)
    .await
    .unwrap();
    row.try_get::<Option<String>, _>("last_thread_id").unwrap()
}

async fn handle_state_json_is_null(pool: &SqlitePool, runtime_id: &str) -> bool {
    let row =
        sqlx::query("SELECT handle_state_json IS NULL AS is_null FROM runtimes WHERE id = ?1")
            .bind(runtime_id)
            .fetch_one(pool)
            .await
            .unwrap();
    row.try_get::<bool, _>("is_null").unwrap()
}

#[tokio::test]
async fn nulls_only_spec_runtime_thread_ids_without_per_card_token_rows() {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    for (name, sql) in MIGRATIONS_UP_TO_0034 {
        apply_sql(&pool, name, sql).await;
    }

    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, created_at, updated_at)
           VALUES ('cove-0035', 'c', '#000000', 0.0, 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    seed_wave(&pool, "wave-spec-token").await;
    seed_wave(&pool, "wave-spec-token-null-state").await;
    seed_wave(&pool, "wave-spec-no-token").await;
    seed_wave(&pool, "wave-spec-no-token-null-state").await;
    seed_wave(&pool, "wave-worker-no-token").await;
    seed_wave(&pool, "wave-plain").await;

    seed_card(&pool, "card-spec-token", "wave-spec-token", Some("spec")).await;
    seed_card(
        &pool,
        "card-spec-token-null-state",
        "wave-spec-token-null-state",
        Some("spec"),
    )
    .await;
    seed_card(
        &pool,
        "card-spec-no-token",
        "wave-spec-no-token",
        Some("spec"),
    )
    .await;
    seed_card(
        &pool,
        "card-spec-no-token-null-state",
        "wave-spec-no-token-null-state",
        Some("spec"),
    )
    .await;
    seed_card(
        &pool,
        "card-worker-no-token",
        "wave-worker-no-token",
        Some("worker"),
    )
    .await;
    seed_card(&pool, "card-plain", "wave-plain", Some("plain")).await;
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at)
           VALUES ('card-spec-token', 'hash-spec-token', 1200)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at)
           VALUES ('card-spec-token-null-state', 'hash-spec-token-null-state', 1200)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    seed_runtime(
        &pool,
        "runtime-spec-token",
        "card-spec-token",
        "thread-A",
        Some(r#"{"mode":"harness","last_thread_id":"thread-A","turn_counter":7}"#),
    )
    .await;
    seed_runtime(
        &pool,
        "runtime-spec-token-null-state",
        "card-spec-token-null-state",
        "thread-A-null",
        None,
    )
    .await;
    seed_runtime(
        &pool,
        "runtime-spec-no-token",
        "card-spec-no-token",
        "thread-B",
        Some(r#"{"mode":"harness","last_thread_id":"thread-B","turn_counter":8}"#),
    )
    .await;
    seed_runtime(
        &pool,
        "runtime-spec-no-token-null-state",
        "card-spec-no-token-null-state",
        "thread-B-null",
        None,
    )
    .await;
    seed_runtime(
        &pool,
        "runtime-worker-no-token",
        "card-worker-no-token",
        "thread-C",
        Some(r#"{"mode":"harness","last_thread_id":"thread-C","turn_counter":9}"#),
    )
    .await;
    seed_runtime(
        &pool,
        "runtime-plain",
        "card-plain",
        "thread-D",
        Some(r#"{"mode":"harness","last_thread_id":"thread-D","turn_counter":10}"#),
    )
    .await;

    apply_sql(
        &pool,
        "0035_pr567_null_pre567_spec_thread_ids",
        MIGRATION_0035_SQL,
    )
    .await;

    assert_eq!(
        thread_id(&pool, "runtime-spec-token").await.as_deref(),
        Some("thread-A")
    );
    assert_eq!(
        snapshot_last_thread_id(&pool, "runtime-spec-token")
            .await
            .as_deref(),
        Some("thread-A")
    );
    assert_eq!(
        thread_id(&pool, "runtime-spec-token-null-state")
            .await
            .as_deref(),
        Some("thread-A-null")
    );
    assert!(handle_state_json_is_null(&pool, "runtime-spec-token-null-state").await);
    assert_eq!(thread_id(&pool, "runtime-spec-no-token").await, None);
    assert_eq!(
        snapshot_last_thread_id(&pool, "runtime-spec-no-token").await,
        None
    );
    assert_eq!(
        thread_id(&pool, "runtime-spec-no-token-null-state").await,
        None
    );
    assert!(handle_state_json_is_null(&pool, "runtime-spec-no-token-null-state").await);
    assert_eq!(
        thread_id(&pool, "runtime-worker-no-token").await.as_deref(),
        Some("thread-C")
    );
    assert_eq!(
        snapshot_last_thread_id(&pool, "runtime-worker-no-token")
            .await
            .as_deref(),
        Some("thread-C")
    );
    assert_eq!(
        thread_id(&pool, "runtime-plain").await.as_deref(),
        Some("thread-D")
    );
    assert_eq!(
        snapshot_last_thread_id(&pool, "runtime-plain")
            .await
            .as_deref(),
        Some("thread-D")
    );
}
