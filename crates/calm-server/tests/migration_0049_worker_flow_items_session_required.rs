//! #695 PR5: migration 0049 preserves pre-PR5 worker flow rows.

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATION_0049_SQL: &str =
    include_str!("../../calm-truth/migrations/0049_worker_flow_items_session_required.sql");

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

async fn stage_pre_0049_schema(pool: &SqlitePool) {
    apply_sql(
        pool,
        "pre-0049",
        r#"
        CREATE TABLE cards (id TEXT PRIMARY KEY);
        CREATE TABLE runtimes (
          id TEXT PRIMARY KEY,
          thread_id TEXT NULL,
          session_id TEXT NULL
        );
        CREATE TABLE worker_sessions (id TEXT PRIMARY KEY);
        CREATE TABLE worker_flow_items (
          id                INTEGER PRIMARY KEY AUTOINCREMENT,
          card_id           TEXT REFERENCES cards(id) ON DELETE SET NULL,
          runtime_id        TEXT,
          wave_id           TEXT,
          worker_session_id TEXT,
          kind              TEXT NOT NULL,
          payload           TEXT NOT NULL,
          created_at_ms     INTEGER NOT NULL
        );
        CREATE INDEX idx_worker_flow_items_card
          ON worker_flow_items(card_id, id);
        CREATE INDEX idx_worker_flow_items_session
          ON worker_flow_items(worker_session_id, id);
        "#,
    )
    .await;
}

async fn seed_flow_item(
    pool: &SqlitePool,
    id: i64,
    card_id: &str,
    runtime_id: Option<&str>,
    worker_session_id: Option<&str>,
    kind: &str,
) {
    sqlx::query(
        r#"INSERT INTO cards (id)
           VALUES (?1)"#,
    )
    .bind(card_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO worker_flow_items
              (id, card_id, runtime_id, wave_id, worker_session_id, kind, payload, created_at_ms)
           VALUES (?1, ?2, ?3, 'wave-0049', ?4, ?5, '{"ok":true}', ?6)"#,
    )
    .bind(id)
    .bind(card_id)
    .bind(runtime_id)
    .bind(worker_session_id)
    .bind(kind)
    .bind(1000 + id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_0049_translates_agent_session_keys_and_preserves_orphans() {
    let pool = fresh_pool().await;
    stage_pre_0049_schema(&pool).await;

    sqlx::query(
        r#"INSERT INTO runtimes (id, thread_id, session_id)
           VALUES
             ('runtime-codex', 'thread-old', NULL),
             ('runtime-claude', NULL, 'session-old'),
             ('runtime-no-worker-session', 'thread-no-worker-session', NULL),
             ('runtime-existing', 'different-thread', NULL)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO worker_sessions (id)
           VALUES ('runtime-codex'), ('runtime-claude'), ('runtime-existing')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    seed_flow_item(&pool, 1, "card-codex", None, Some("thread-old"), "codex").await;
    seed_flow_item(&pool, 2, "card-claude", None, Some("session-old"), "claude").await;
    seed_flow_item(
        &pool,
        3,
        "card-orphan",
        None,
        Some("missing-agent-session"),
        "orphan",
    )
    .await;
    seed_flow_item(
        &pool,
        4,
        "card-runtime-no-session",
        None,
        Some("thread-no-worker-session"),
        "runtime-no-session",
    )
    .await;
    seed_flow_item(
        &pool,
        5,
        "card-existing",
        Some("runtime-existing"),
        Some("thread-old"),
        "existing",
    )
    .await;

    apply_sql(
        &pool,
        "0049_worker_flow_items_session_required",
        MIGRATION_0049_SQL,
    )
    .await;

    let rows = sqlx::query(
        r#"SELECT id, runtime_id, worker_session_id, kind
           FROM worker_flow_items
           ORDER BY id"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 5);

    let projected: Vec<(i64, Option<String>, Option<String>, String)> = rows
        .into_iter()
        .map(|row| {
            (
                row.get("id"),
                row.get("runtime_id"),
                row.get("worker_session_id"),
                row.get("kind"),
            )
        })
        .collect();

    assert_eq!(
        projected,
        vec![
            (
                1,
                Some("runtime-codex".into()),
                Some("runtime-codex".into()),
                "codex".into()
            ),
            (
                2,
                Some("runtime-claude".into()),
                Some("runtime-claude".into()),
                "claude".into()
            ),
            (3, None, None, "orphan".into()),
            (
                4,
                Some("runtime-no-worker-session".into()),
                None,
                "runtime-no-session".into()
            ),
            (
                5,
                Some("runtime-existing".into()),
                Some("runtime-existing".into()),
                "existing".into()
            ),
        ]
    );

    let fk = sqlx::query(
        r#"SELECT "table", "from", "to", on_delete
           FROM pragma_foreign_key_list('worker_flow_items')
           WHERE "from" = 'worker_session_id'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fk.get::<String, _>("table"), "worker_sessions");
    assert_eq!(fk.get::<String, _>("to"), "id");
    assert_eq!(fk.get::<String, _>("on_delete"), "SET NULL");
}
