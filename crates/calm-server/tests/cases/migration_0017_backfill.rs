//! Migration 0017 backfill + NOT NULL invariant test. Migrations 0001..0016 are replayed by hand so the
//! DB can be staged exactly one step short of 0017, which `sqlx::migrate!()` cannot do.

#![cfg(unix)]

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

/// All migrations up to and including 0016, in order.
const MIGRATIONS_UP_TO_0016: &[(&str, &str)] = &[
    (
        "0001_init",
        include_str!("../../../calm-truth/migrations/0001_init.sql"),
    ),
    (
        "0002_plugins",
        include_str!("../../../calm-truth/migrations/0002_plugins.sql"),
    ),
    (
        "0003_settings",
        include_str!("../../../calm-truth/migrations/0003_settings.sql"),
    ),
    (
        "0004_events",
        include_str!("../../../calm-truth/migrations/0004_events.sql"),
    ),
    (
        "0005_terminals_pid",
        include_str!("../../../calm-truth/migrations/0005_terminals_pid.sql"),
    ),
    (
        "0006_events_version",
        include_str!("../../../calm-truth/migrations/0006_events_version.sql"),
    ),
    (
        "0007_events_scope",
        include_str!("../../../calm-truth/migrations/0007_events_scope.sql"),
    ),
    (
        "0008_cards_role",
        include_str!("../../../calm-truth/migrations/0008_cards_role.sql"),
    ),
    (
        "0009_coves_kind",
        include_str!("../../../calm-truth/migrations/0009_coves_kind.sql"),
    ),
    (
        "0010_card_mcp_tokens",
        include_str!("../../../calm-truth/migrations/0010_card_mcp_tokens.sql"),
    ),
    (
        "0011_terminals_card_id_restrict",
        include_str!("../../../calm-truth/migrations/0011_terminals_card_id_restrict.sql"),
    ),
    (
        "0012_waves_lifecycle",
        include_str!("../../../calm-truth/migrations/0012_waves_lifecycle.sql"),
    ),
    (
        "0013_cards_deletable",
        include_str!("../../../calm-truth/migrations/0013_cards_deletable.sql"),
    ),
    (
        "0014_wave_report_card",
        include_str!("../../../calm-truth/migrations/0014_wave_report_card.sql"),
    ),
    (
        "0015_cove_folders",
        include_str!("../../../calm-truth/migrations/0015_cove_folders.sql"),
    ),
    (
        "0016_terminals_theme",
        include_str!("../../../calm-truth/migrations/0016_terminals_theme.sql"),
    ),
];

/// The verbatim SQL from `migrations/0017_terminals_theme_not_null.sql`.
const MIGRATION_0017_SQL: &str =
    include_str!("../../../calm-truth/migrations/0017_terminals_theme_not_null.sql");

/// Strip `--` line comments then split on top-level `;` and execute each non-empty statement.
async fn apply_sql(pool: &SqlitePool, name: &str, sql: &str) {
    let stripped: String = sql
        .lines()
        .map(|l| match l.find("--") {
            Some(idx) => &l[..idx],
            None => l,
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

/// Fresh in-memory pool with `foreign_keys = ON` (matches the production `SqlxRepo::open` pragma), replaying 0001..=0016 by hand.
async fn pool_staged_at_0016() -> SqlitePool {
    // `sqlite::memory:` would give each pooled connection its own DB; a shared-cache named URI makes
    // them all see the same one, and a unique name avoids leakage across tests in the same process.
    let unique = format!(
        "file:mig0017_{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4().simple()
    );
    let opts = SqliteConnectOptions::from_str(&unique)
        .expect("parse opts")
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.expect("connect");
    for (name, sql) in MIGRATIONS_UP_TO_0016 {
        apply_sql(&pool, name, sql).await;
    }
    pool
}

/// `card_id` is `NOT NULL UNIQUE REFERENCES cards(id)`, so an area → track → card chain is minted before the terminal.
async fn seed_terminal_with_null_theme(pool: &SqlitePool) -> String {
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, created_at, updated_at)
           VALUES ('area-1', 'c', '#000', 0.0, 0, 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO waves
               (id, cove_id, title, sort, archived_at, created_at, updated_at)
           VALUES ('track-1', 'area-1', 'w', 0.0, NULL, 0, 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    // Inserting only the columns we know about keeps the test resilient to additive migrations between 0008 and 0016.
    sqlx::query(
        r#"INSERT INTO cards
               (id, wave_id, kind, sort, payload, created_at, updated_at)
           VALUES ('card-1', 'track-1', 'terminal', 0.0, '{}', 0, 0)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES ('term-1', 'card-1', '/bin/sh', '/tmp', '{}',
                   NULL, NULL, NULL, 0)"#,
    )
    .execute(pool)
    .await
    .expect("INSERT terminals with NULL theme legal at 0016");
    "term-1".to_string()
}

#[tokio::test]
async fn backfill_stamps_dark_default_on_pre_177_rows() {
    let pool = pool_staged_at_0016().await;
    let term_id = seed_terminal_with_null_theme(&pool).await;

    let pre = sqlx::query("SELECT theme_fg, theme_bg FROM terminals WHERE id = ?1")
        .bind(&term_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let pre_fg: Option<String> = pre.try_get("theme_fg").unwrap();
    let pre_bg: Option<String> = pre.try_get("theme_bg").unwrap();
    assert_eq!(pre_fg, None, "row's theme_fg is NULL at 0016");
    assert_eq!(pre_bg, None, "row's theme_bg is NULL at 0016");

    apply_sql(&pool, "0017_terminals_theme_not_null", MIGRATION_0017_SQL).await;

    // The literals must match the `UPDATE` clauses in the migration *and* `RequestTheme::default_dark()`.
    let post = sqlx::query("SELECT theme_fg, theme_bg FROM terminals WHERE id = ?1")
        .bind(&term_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let post_fg: String = post.try_get("theme_fg").unwrap();
    let post_bg: String = post.try_get("theme_bg").unwrap();
    assert_eq!(post_fg, "216,219,226", "fg backfilled to dark default");
    assert_eq!(post_bg, "15,20,24", "bg backfilled to dark default");
}

#[tokio::test]
async fn post_0017_schema_rejects_null_theme_inserts() {
    let pool = pool_staged_at_0016().await;
    // Reuses the helper's terminal row but deletes it: only the FK-anchor cards row is needed.
    let _ = seed_terminal_with_null_theme(&pool).await;
    sqlx::query("DELETE FROM terminals WHERE id = 'term-1'")
        .execute(&pool)
        .await
        .unwrap();

    apply_sql(&pool, "0017_terminals_theme_not_null", MIGRATION_0017_SQL).await;

    let res = sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES ('term-2', 'card-1', '/bin/sh', '/tmp', '{}',
                   NULL, NULL, NULL, 0)"#,
    )
    .execute(&pool)
    .await;
    assert!(
        res.is_err(),
        "post-0017 NOT NULL constraint must reject NULL theme; got {res:?}"
    );
    let err_msg = format!("{:?}", res.unwrap_err()).to_ascii_lowercase();
    assert!(
        err_msg.contains("not null") || err_msg.contains("constraint"),
        "error message looks like a NOT NULL violation: {err_msg}"
    );

    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES ('term-3', 'card-1', '/bin/sh', '/tmp', '{}',
                   NULL, '216,219,226', '15,20,24', 0)"#,
    )
    .execute(&pool)
    .await
    .expect("insert with concrete theme strings succeeds post-0017");
}
