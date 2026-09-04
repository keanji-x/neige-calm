//! #1316 S4b — migration 0094 retires the `runtime_id` spelling.
//!
//! Every test here is written to FAIL against a specific defective version of
//! the migration, not merely to observe the happy path:
//!
//!   * `rewrites_runtime_started_kind_version_and_payload_key` fails if the
//!     kind, the `event_version` stamp, or the payload key is left behind —
//!     and its negative-control row (a payload whose *value* contains the
//!     literal text `runtime_id`) fails if the rewrite is done with a string
//!     `replace()` instead of JSON1.
//!   * `guard_does_not_fabricate_key_on_rows_without_it` fails if any
//!     statement drops its existence guard: an unguarded `json_set` writes
//!     `worker_session_id: null` onto rows that never carried the key.
//!   * `invalid_json_payload_does_not_abort_the_migration` fails if the
//!     `json_valid` guard is written as an `AND` conjunct (or omitted):
//!     `json_extract` on a non-JSON body aborts the whole file.
//!   * `operations_rows_are_untouched` fails if anyone adds an
//!     `UPDATE operations` — see 0094 §4: the three JSON columns have
//!     different path shapes, and the wire key is pinned in Rust with
//!     `#[serde(rename)]` instead.
//!   * `no_harness_items_index_named_after_runtime_survives` fails if the
//!     retired index name comes back (`RENAME COLUMN` would have rewritten
//!     its definition and kept the name).

use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::str::FromStr;

const MIGRATION_0094_SQL: &str =
    include_str!("../../../calm-truth/migrations/0094_runtime_id_to_worker_session_id.sql");

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

/// The three tables 0094 reads, at their pre-0094 shape. `events.payload` has
/// NO `json_valid` CHECK (that is the whole point of the CASE guard);
/// `operations`' three JSON columns do, exactly as `0029`/`0042` declare them.
async fn stage_pre_0094_schema(pool: &SqlitePool) {
    apply_sql(
        pool,
        "pre-0094",
        r#"
        CREATE TABLE events (
          id            INTEGER PRIMARY KEY AUTOINCREMENT,
          kind          TEXT    NOT NULL,
          payload       TEXT    NOT NULL,
          actor         TEXT    NOT NULL,
          at            INTEGER NOT NULL,
          correlation   TEXT,
          event_version INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE harness_items (
          id            INTEGER PRIMARY KEY AUTOINCREMENT,
          runtime_id    TEXT    NOT NULL,
          card_id       TEXT    NOT NULL,
          track_id      TEXT    NOT NULL,
          thread_id     TEXT    NOT NULL,
          turn_id       TEXT,
          item_uuid     TEXT,
          item_type     TEXT,
          method        TEXT    NOT NULL,
          params        TEXT    NOT NULL,
          created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX idx_harness_items_runtime_id ON harness_items(runtime_id, id);
        CREATE INDEX idx_harness_items_card_id    ON harness_items(card_id, id);
        CREATE TABLE operations (
          id                 TEXT PRIMARY KEY,
          kind               TEXT NOT NULL,
          payload_json       TEXT NOT NULL CHECK (json_valid(payload_json)),
          tx_output_json     TEXT NULL CHECK (tx_output_json IS NULL OR json_valid(tx_output_json)),
          compensation_state TEXT NULL CHECK (compensation_state IS NULL OR json_valid(compensation_state))
        );
        "#,
    )
    .await;
}

async fn insert_event(pool: &SqlitePool, kind: &str, payload: &str, version: i64) -> i64 {
    sqlx::query(
        "INSERT INTO events (kind, payload, actor, at, event_version)
         VALUES (?1, ?2, 'kernel', 1000, ?3) RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .bind(version)
    .fetch_one(pool)
    .await
    .unwrap()
    .get::<i64, _>("id")
}

async fn event_row(pool: &SqlitePool, id: i64) -> (String, String, i64) {
    let row = sqlx::query("SELECT kind, payload, event_version FROM events WHERE id = ?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    (
        row.get::<String, _>("kind"),
        row.get::<String, _>("payload"),
        row.get::<i64, _>("event_version"),
    )
}

#[tokio::test]
async fn rewrites_runtime_started_kind_version_and_payload_key() {
    let pool = fresh_pool().await;
    stage_pre_0094_schema(&pool).await;

    let started = insert_event(
        &pool,
        "runtime.started",
        r#"{"runtime_id":"rt-1","card_id":"card-1","kind":"codex","agent_provider":"codex","status":"starting"}"#,
        15,
    )
    .await;
    let superseded = insert_event(
        &pool,
        "runtime.superseded",
        r#"{"old_runtime_id":"rt-1","new_runtime_id":"rt-2","card_id":"card-1"}"#,
        15,
    )
    .await;
    let harness = insert_event(
        &pool,
        "harness.user_message.enqueued",
        r#"{"runtime_id":"rt-1","card_id":"card-1","track_id":"track-1","char_count":3}"#,
        15,
    )
    .await;
    let card = insert_event(
        &pool,
        "card.added",
        r#"{"id":"card-1","track_id":"track-1","kind":"codex","sort":1.0,"payload":{},"runtime":{"runtime_id":"rt-1","kind":"codex","status":"running"},"deletable":false,"created_at":1,"updated_at":2}"#,
        15,
    )
    .await;

    // NEGATIVE CONTROL. A different kind entirely, whose payload merely
    // *mentions* the word inside a string VALUE. A `replace(payload,
    // 'runtime_id', ...)` rewrite would corrupt this; JSON1 cannot.
    let unrelated = insert_event(
        &pool,
        "track.report_edited",
        r#"{"author":"planner","summary":"renamed runtime_id to worker_session_id in the design doc"}"#,
        15,
    )
    .await;

    apply_sql(&pool, "0094", MIGRATION_0094_SQL).await;

    let (kind, payload, version) = event_row(&pool, started).await;
    assert_eq!(kind, "worker_session.started");
    assert_eq!(version, 16, "kind rewrite must carry the version stamp");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["worker_session_id"], "rt-1");
    assert!(
        json.get("runtime_id").is_none(),
        "old key survived: {payload}"
    );

    let (kind, payload, version) = event_row(&pool, superseded).await;
    assert_eq!(kind, "worker_session.superseded");
    assert_eq!(version, 16);
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["old_worker_session_id"], "rt-1");
    assert_eq!(json["new_worker_session_id"], "rt-2");
    assert!(json.get("old_runtime_id").is_none(), "{payload}");
    assert!(json.get("new_runtime_id").is_none(), "{payload}");

    let (kind, payload, version) = event_row(&pool, harness).await;
    assert_eq!(
        kind, "harness.user_message.enqueued",
        "kind must NOT change"
    );
    assert_eq!(version, 16);
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["worker_session_id"], "rt-1");
    assert!(json.get("runtime_id").is_none(), "{payload}");

    let (kind, payload, version) = event_row(&pool, card).await;
    assert_eq!(kind, "card.added");
    assert_eq!(version, 16);
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        json["runtime"]["worker_session_id"], "rt-1",
        "the card key is nested under $.runtime, not at the root: {payload}"
    );
    assert!(json["runtime"].get("runtime_id").is_none(), "{payload}");
    assert!(
        json.get("worker_session_id").is_none(),
        "a root-level key was fabricated on a card payload: {payload}"
    );

    let (kind, payload, version) = event_row(&pool, unrelated).await;
    assert_eq!(kind, "track.report_edited");
    assert_eq!(
        version, 15,
        "an untouched row must not have its event_version advanced"
    );
    assert!(
        payload.contains("renamed runtime_id to worker_session_id"),
        "the negative control's string value was rewritten: {payload}"
    );
}

#[tokio::test]
async fn guard_does_not_fabricate_key_on_rows_without_it() {
    let pool = fresh_pool().await;
    stage_pre_0094_schema(&pool).await;

    // A card event with no `runtime` object at all — the common case, since
    // `Card.runtime` is Optional and skipped when absent.
    let card_no_runtime = insert_event(
        &pool,
        "card.updated",
        r#"{"id":"card-1","track_id":"track-1","kind":"terminal","sort":1.0,"payload":{},"deletable":true,"created_at":1,"updated_at":2}"#,
        15,
    )
    .await;
    // A harness event that somehow lacks the key.
    let harness_no_key = insert_event(
        &pool,
        "harness.phase.changed",
        r#"{"card_id":"card-1","track_id":"track-1","old_phase":"idle","new_phase":"turn_running"}"#,
        15,
    )
    .await;
    // A superseded row holding only ONE of its two ids: the other must not be
    // conjured as null by the sibling statement.
    let half_superseded = insert_event(
        &pool,
        "runtime.superseded",
        r#"{"old_runtime_id":"rt-1","card_id":"card-1"}"#,
        15,
    )
    .await;

    apply_sql(&pool, "0094", MIGRATION_0094_SQL).await;

    let (_, payload, version) = event_row(&pool, card_no_runtime).await;
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(
        json.get("runtime").is_none() && json.get("worker_session_id").is_none(),
        "a key was fabricated on a card row with no runtime view: {payload}"
    );
    assert_eq!(version, 15, "unchanged row must keep its event_version");

    let (_, payload, version) = event_row(&pool, harness_no_key).await;
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(
        json.get("worker_session_id").is_none(),
        "a key was fabricated on a harness row that never had one: {payload}"
    );
    assert_eq!(version, 15);

    let (_, payload, _) = event_row(&pool, half_superseded).await;
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["old_worker_session_id"], "rt-1");
    assert!(
        json.get("new_worker_session_id").is_none(),
        "the absent second id was fabricated: {payload}"
    );
}

#[tokio::test]
async fn invalid_json_payload_does_not_abort_the_migration() {
    let pool = fresh_pool().await;
    stage_pre_0094_schema(&pool).await;

    // `events.payload` has no json_valid CHECK, so this row is insertable —
    // and `json_extract` on it aborts an unguarded statement.
    let junk = insert_event(&pool, "harness.item.added", "not json at all", 15).await;
    let good = insert_event(
        &pool,
        "harness.item.added",
        r#"{"runtime_id":"rt-1","card_id":"card-1","track_id":"track-1","item_db_id":1,"method":"item/completed"}"#,
        15,
    )
    .await;

    // The assertion is that this does not panic.
    apply_sql(&pool, "0094", MIGRATION_0094_SQL).await;

    let (_, payload, version) = event_row(&pool, junk).await;
    assert_eq!(payload, "not json at all", "the junk row was rewritten");
    assert_eq!(version, 15);

    let (_, payload, version) = event_row(&pool, good).await;
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        json["worker_session_id"], "rt-1",
        "a junk row in the same table blocked a valid row's rewrite"
    );
    assert_eq!(version, 16);
}

#[tokio::test]
async fn operations_rows_are_untouched() {
    let pool = fresh_pool().await;
    stage_pre_0094_schema(&pool).await;

    sqlx::query(
        "INSERT INTO operations (id, kind, payload_json, tx_output_json, compensation_state)
         VALUES ('op-empty', 'spawn_worker', '{}', NULL, NULL),
                ('op-full', 'planner-harness-interrupt',
                 '{\"runtime_id\":\"rt-1\",\"reason\":\"stop\"}',
                 '{\"target_type\":\"runtime\",\"data\":{\"runtime_id\":\"rt-1\"}}',
                 '{\"version\":1,\"steps\":[{\"op\":\"x\",\"args\":{\"runtime_id\":\"rt-1\"}}]}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_sql(&pool, "0094", MIGRATION_0094_SQL).await;

    let rows = sqlx::query(
        "SELECT id, payload_json, tx_output_json, compensation_state
           FROM operations ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);

    assert_eq!(rows[0].get::<String, _>("id"), "op-empty");
    assert_eq!(
        rows[0].get::<String, _>("payload_json"),
        "{}",
        "0094 rewrote an operations payload of {{}} — see 0094 §4"
    );

    assert_eq!(rows[1].get::<String, _>("id"), "op-full");
    for column in ["payload_json", "tx_output_json", "compensation_state"] {
        let value: String = rows[1].get(column);
        assert!(
            value.contains("runtime_id") && !value.contains("worker_session_id"),
            "operations.{column} was rewritten by 0094: {value}"
        );
    }
}

#[tokio::test]
async fn no_harness_items_index_named_after_runtime_survives() {
    let pool = fresh_pool().await;
    stage_pre_0094_schema(&pool).await;
    apply_sql(&pool, "0094", MIGRATION_0094_SQL).await;

    // The column really did move.
    let columns: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info('harness_items')")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();
    assert!(
        columns.iter().any(|c| c == "worker_session_id"),
        "{columns:?}"
    );
    assert!(!columns.iter().any(|c| c == "runtime_id"), "{columns:?}");

    let indexes: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'harness_items'",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>("name"))
    .collect();
    assert!(
        !indexes.iter().any(|name| name.contains("runtime")),
        "an index whose NAME encodes the retired word survived: {indexes:?}"
    );
    assert!(
        indexes
            .iter()
            .any(|name| name == "idx_harness_items_card_id"),
        "the card_id index — the one every query actually uses — is gone: {indexes:?}"
    );
}
