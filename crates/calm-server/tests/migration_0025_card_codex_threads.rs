//! PR2 (#410) — migration 0025 backfill and constraints.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATIONS_UP_TO_0024: &[(&str, &str)] = &[
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
];

const MIGRATION_0025_SQL: &str = include_str!("../migrations/0025_card_codex_threads.sql");

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

async fn pool_staged_at_0024() -> SqlitePool {
    let unique = format!(
        "file:mig0025_{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4().simple()
    );
    let opts = SqliteConnectOptions::from_str(&unique)
        .expect("parse opts")
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.expect("connect");
    for (name, sql) in MIGRATIONS_UP_TO_0024 {
        apply_sql(&pool, name, sql).await;
    }
    pool
}

async fn seed_wave_and_cards(pool: &SqlitePool) {
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
           VALUES ('wave-1', 'cove-1', 'w', 0.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role, deletable)
           VALUES
              ('card-1', 'wave-1', 'codex', 0.0, '{"codex_thread_id":"thread-old"}', 1100, 1100, 'spec', 0),
              ('card-2', 'wave-1', 'codex', 1.0, '{}', 1200, 1200, 'worker', 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_backfills_payload_codex_thread_id() {
    let pool = pool_staged_at_0024().await;
    seed_wave_and_cards(&pool).await;

    apply_sql(&pool, "0025_card_codex_threads", MIGRATION_0025_SQL).await;

    let row = sqlx::query(
        "SELECT thread_id, card_id, role, wave_id, created_at, updated_at FROM card_codex_threads",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("thread_id").unwrap(), "thread-old");
    assert_eq!(row.try_get::<String, _>("card_id").unwrap(), "card-1");
    assert_eq!(row.try_get::<String, _>("role").unwrap(), "spec");
    assert_eq!(row.try_get::<String, _>("wave_id").unwrap(), "wave-1");
    assert_eq!(row.try_get::<i64, _>("created_at").unwrap(), 1100);
    assert_eq!(row.try_get::<i64, _>("updated_at").unwrap(), 1100);
}

#[tokio::test]
async fn migration_does_not_backfill_plain_card_payloads() {
    let pool = pool_staged_at_0024().await;

    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES ('cove-plain', 'c', '#000000', 0.0, 'user', 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO waves
              (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES ('wave-plain', 'cove-plain', 'w', 0.0, NULL, NULL, 'draft', '/tmp', NULL, 1000, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role, deletable)
           VALUES
              ('card-plain-1', 'wave-plain', 'plugin', 0.0, '{"schemaVersion":1,"codex_thread_id":"plugin-thread-A"}', 1100, 1100, 'plain', 1),
              ('card-plain-2', 'wave-plain', 'plugin', 1.0, '{"schemaVersion":1,"codex_thread_id":"plugin-thread-A"}', 1200, 1200, 'plain', 1),
              ('card-spec-1', 'wave-plain', 'codex', 2.0, '{"schemaVersion":1,"codex_thread_id":"real-thread-X"}', 1300, 1300, 'spec', 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0025_card_codex_threads", MIGRATION_0025_SQL).await;

    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT thread_id, card_id, role FROM card_codex_threads ORDER BY card_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "only spec backfilled: {rows:?}");
    assert_eq!(rows[0].0, "real-thread-X");
    assert_eq!(rows[0].1, "card-spec-1");
    assert_eq!(rows[0].2, "spec");
}

#[tokio::test]
async fn migration_enforces_unique_thread_and_card() {
    let pool = pool_staged_at_0024().await;
    seed_wave_and_cards(&pool).await;
    apply_sql(&pool, "0025_card_codex_threads", MIGRATION_0025_SQL).await;

    let duplicate_thread = sqlx::query(
        r#"INSERT INTO card_codex_threads
              (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES ('thread-old', 'card-2', 'worker', 'wave-1', 1300, 1300)"#,
    )
    .execute(&pool)
    .await;
    assert!(
        duplicate_thread.is_err(),
        "PRIMARY KEY(thread_id) must reject sharing one thread across cards"
    );

    let duplicate_card = sqlx::query(
        r#"INSERT INTO card_codex_threads
              (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES ('thread-new', 'card-1', 'spec', 'wave-1', 1400, 1400)"#,
    )
    .execute(&pool)
    .await;
    assert!(
        duplicate_card.is_err(),
        "UNIQUE(card_id) must reject assigning two threads to one card"
    );
}

#[tokio::test]
async fn migration_cascades_on_card_delete() {
    let pool = pool_staged_at_0024().await;
    seed_wave_and_cards(&pool).await;
    apply_sql(&pool, "0025_card_codex_threads", MIGRATION_0025_SQL).await;

    sqlx::query("DELETE FROM cards WHERE id = 'card-1'")
        .execute(&pool)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_codex_threads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn migration_0025_card_codex_threads() {
    let pool = pool_staged_at_0024().await;
    seed_wave_and_cards(&pool).await;
    apply_sql(&pool, "0025_card_codex_threads", MIGRATION_0025_SQL).await;

    let backfilled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM card_codex_threads WHERE thread_id = 'thread-old'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(backfilled, 1);

    let duplicate_thread = sqlx::query(
        r#"INSERT INTO card_codex_threads
              (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES ('thread-old', 'card-2', 'worker', 'wave-1', 1300, 1300)"#,
    )
    .execute(&pool)
    .await;
    assert!(duplicate_thread.is_err());

    sqlx::query("DELETE FROM cards WHERE id = 'card-1'")
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM card_codex_threads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
