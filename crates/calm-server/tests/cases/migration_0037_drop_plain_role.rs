//! #585: migration 0037 drops the legacy plain card role.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

macro_rules! migration {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../../calm-truth/migrations/", $name, ".sql")),
        )
    };
}

const MIGRATIONS_UP_TO_0036: &[(&str, &str)] = &[
    migration!("0001_init"),
    migration!("0002_plugins"),
    migration!("0003_settings"),
    migration!("0004_events"),
    migration!("0005_terminals_pid"),
    migration!("0006_events_version"),
    migration!("0007_events_scope"),
    migration!("0008_cards_role"),
    migration!("0009_coves_kind"),
    migration!("0010_card_mcp_tokens"),
    migration!("0011_terminals_card_id_restrict"),
    migration!("0012_waves_lifecycle"),
    migration!("0013_cards_deletable"),
    migration!("0014_wave_report_card"),
    migration!("0015_cove_folders"),
    migration!("0016_terminals_theme"),
    migration!("0017_terminals_theme_not_null"),
    migration!("0018_wave_cwd_terminal_at"),
    migration!("0019_cards_body_crdt"),
    migration!("0020_terminals_exit_code"),
    migration!("0021_waves_pinned_at"),
    migration!("0022_spec_push_queue"),
    migration!("0023_phase3b_proc_handle"),
    migration!("0024_drop_terminals_daemon_handle"),
    migration!("0025_card_codex_threads"),
    migration!("0026_shared_codex_daemon"),
    migration!("0027_shared_daemon_env_signature"),
    migration!("0028_runtimes"),
    migration!("0029_operations"),
    migration!("0030_runtimes_backfill"),
    migration!("0031_harness_items"),
    migration!("0032_harness_items_fk"),
    migration!("0033_drop_spec_push_queue"),
    migration!("0034_drop_card_codex_threads"),
    migration!("0035_pr567_null_pre567_spec_thread_ids"),
    migration!("0036_pr570_clear_pre567_spec_snapshot_last_thread_id"),
];

const MIGRATION_0037_SQL: &str =
    include_str!("../../../calm-truth/migrations/0037_drop_plain_role.sql");
const VALIDATION_MESSAGE: &str = "cards.role must be one of worker|spec|reportcard (#585)";

async fn apply_sql(pool: &SqlitePool, name: &str, sql: &str) {
    let stripped = sql
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    for stmt in split_sql_statements(&stripped) {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("migration {name} failed on stmt:\n{trimmed}\nerror: {e}"));
    }
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_trigger = false;

    for line in sql.lines() {
        let trimmed = line.trim();
        if !in_trigger
            && current.trim().is_empty()
            && trimmed.to_ascii_uppercase().starts_with("CREATE TRIGGER ")
        {
            in_trigger = true;
        }

        if in_trigger {
            current.push_str(line);
            current.push('\n');
            if trimmed.eq_ignore_ascii_case("END;") {
                statements.push(current.trim().to_string());
                current.clear();
                in_trigger = false;
            }
            continue;
        }

        for part in line.split_inclusive(';') {
            current.push_str(part);
            if part.ends_with(';') {
                statements.push(current.trim_end_matches(';').trim().to_string());
                current.clear();
            }
        }
        current.push('\n');
    }

    if !current.trim().is_empty() {
        statements.push(current.trim().to_string());
    }
    statements
}

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    SqlitePool::connect_with(opts).await.unwrap()
}

async fn stage_to_0036(pool: &SqlitePool) {
    for (name, sql) in MIGRATIONS_UP_TO_0036 {
        apply_sql(pool, name, sql).await;
    }
}

async fn stage_to_0037(pool: &SqlitePool) {
    stage_to_0036(pool).await;
    apply_sql(pool, "0037_drop_plain_role", MIGRATION_0037_SQL).await;
}

async fn seed_wave(pool: &SqlitePool, wave_id: &str) {
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, created_at, updated_at)
           VALUES ('cove-0037', 'c', '#000000', 0.0, 1000, 1000)"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO waves
              (id, cove_id, title, sort, archived_at, created_at, updated_at)
           VALUES (?1, 'cove-0037', 'w', 0.0, NULL, 1000, 1000)"#,
    )
    .bind(wave_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_card(pool: &SqlitePool, card_id: &str, wave_id: &str, role: &str) {
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

async fn card_role(pool: &SqlitePool, card_id: &str) -> String {
    let row = sqlx::query("SELECT role FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_one(pool)
        .await
        .unwrap();
    row.try_get::<String, _>("role").unwrap()
}

fn assert_role_validation_error(error: sqlx::Error) {
    let message = error.to_string();
    assert!(
        message.contains(VALIDATION_MESSAGE),
        "expected role validation error, got: {message}"
    );
}

#[tokio::test]
async fn migration_0037_backfills_plain_to_worker() {
    let pool = fresh_pool().await;
    stage_to_0036(&pool).await;
    seed_wave(&pool, "wave-0037").await;
    seed_card(&pool, "card-plain", "wave-0037", "plain").await;
    seed_card(&pool, "card-spec", "wave-0037", "spec").await;
    seed_card(&pool, "card-worker", "wave-0037", "worker").await;

    apply_sql(&pool, "0037_drop_plain_role", MIGRATION_0037_SQL).await;

    assert_eq!(card_role(&pool, "card-plain").await, "worker");
    assert_eq!(card_role(&pool, "card-spec").await, "spec");
    assert_eq!(card_role(&pool, "card-worker").await, "worker");
}

#[tokio::test]
async fn migration_0037_rejects_plain_insert_after_apply() {
    let pool = fresh_pool().await;
    stage_to_0037(&pool).await;
    seed_wave(&pool, "wave-0037").await;

    let result = sqlx::query(
        r#"INSERT INTO cards
              (id, wave_id, kind, sort, payload, created_at, updated_at, role)
           VALUES ('card-plain', 'wave-0037', 'codex', 0.0, '{}', 1000, 1000, 'plain')"#,
    )
    .execute(&pool)
    .await;

    assert_role_validation_error(result.unwrap_err());
}

#[tokio::test]
async fn migration_0037_rejects_plain_update_after_apply() {
    let pool = fresh_pool().await;
    stage_to_0037(&pool).await;
    seed_wave(&pool, "wave-0037").await;
    seed_card(&pool, "card-worker", "wave-0037", "worker").await;

    let result = sqlx::query("UPDATE cards SET role = 'plain' WHERE id = 'card-worker'")
        .execute(&pool)
        .await;

    assert_role_validation_error(result.unwrap_err());
}
