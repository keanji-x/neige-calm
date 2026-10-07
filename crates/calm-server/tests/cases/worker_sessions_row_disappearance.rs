//! The set of ways a `worker_sessions` row can disappear, asked of SQLite rather than re-derived from migration text.
//! Foreign keys come from `PRAGMA foreign_key_list`; triggers are matched textually because SQLite has no pragma for a trigger's write targets.

use crate::support::migration_replay::replay_to_head;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

/// One foreign key somewhere in the schema that points at `worker_sessions`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReferenceToWorkerSessions {
    from_table: String,
    from_column: String,
    on_delete: String,
}

/// Every foreign key in the database that references `worker_sessions`, asked of
/// SQLite table by table.
async fn references_to_worker_sessions(pool: &SqlitePool) -> Vec<ReferenceToWorkerSessions> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("list tables");

    let mut found = Vec::new();
    for table in tables {
        // PRAGMA takes an identifier, not a bound value, so the name (from `sqlite_master`) is quoted.
        let rows = sqlx::query(&format!(r#"PRAGMA foreign_key_list("{table}")"#))
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| panic!("foreign_key_list({table}): {e}"));
        for row in rows {
            let referenced: String = row.try_get("table").expect("fk target table");
            if referenced != "worker_sessions" {
                continue;
            }
            found.push(ReferenceToWorkerSessions {
                from_table: table.clone(),
                from_column: row.try_get("from").expect("fk source column"),
                on_delete: row.try_get("on_delete").expect("fk on_delete action"),
            });
        }
    }
    found.sort();
    found
}

/// Every trigger whose body deletes from `worker_sessions`; textual, because SQLite has no pragma for a trigger's write targets.
async fn triggers_deleting_from_worker_sessions(pool: &SqlitePool) -> Vec<String> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .fetch_all(pool)
            .await
            .expect("list triggers");
    let mut found: Vec<String> = rows
        .into_iter()
        .filter(|(_, sql)| {
            let Some(sql) = sql else { return false };
            let normalized = sql
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            normalized.contains("delete from worker_sessions")
                || normalized.contains("delete from \"worker_sessions\"")
        })
        .map(|(name, _)| name)
        .collect();
    found.sort();
    found
}

async fn head_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    replay_to_head(&pool).await;
    pool
}

/// Freeze incoming child ownership and prohibit triggers deleting the carrier.
/// An incoming CASCADE deletes child metadata after carrier deletion; it cannot
/// delete the carrier. Every allowed child edge still needs an inventory entry.
#[tokio::test]
async fn nothing_in_the_schema_deletes_a_worker_sessions_row_behind_its_owner() {
    let pool = head_pool().await;

    let references = references_to_worker_sessions(&pool).await;
    let cascading = unregistered_cascading_children(&references);
    assert!(
        cascading.is_empty(),
        "an unregistered child now cascades deletion from worker_sessions: {cascading:#?}"
    );

    let triggers = triggers_deleting_from_worker_sessions(&pool).await;
    assert!(
        triggers.is_empty(),
        "a trigger now deletes from worker_sessions: {triggers:#?}"
    );

    // The full inventory, frozen: a changed action or a new reference is a change to the reasoning.
    insta_like_assert(&references);
}

/// The expectation, spelled out rather than snapshotted, so the diff shows what
/// changed and the reader sees the actions without opening another file.
fn insta_like_assert(references: &[ReferenceToWorkerSessions]) {
    let actual: Vec<(String, String, String)> = references
        .iter()
        .map(|r| {
            (
                r.from_table.clone(),
                r.from_column.clone(),
                r.on_delete.clone(),
            )
        })
        .collect();
    let expected: Vec<(String, String, String)> = EXPECTED_REFERENCES
        .iter()
        .map(|(t, c, a)| ((*t).to_owned(), (*c).to_owned(), (*a).to_owned()))
        .collect();
    assert_eq!(
        actual, expected,
        "the set of foreign keys pointing at worker_sessions drifted"
    );
}

/// Incoming keys act on children when the worker is deleted, never delete the worker.
/// ACP ownership and receipts are lifecycle-owned children; deletion removes their
/// metadata with the worker. Freeze these exact edges, not an open cascade exemption.
const EXPECTED_REFERENCES: &[(&str, &str, &str)] = &[
    ("acp_managed_sessions", "worker_session_id", "CASCADE"),
    ("acp_submissions", "worker_session_id", "CASCADE"),
    ("cards", "session_id", "SET NULL"),
    ("tracks", "root_session_id", "NO ACTION"),
    ("worker_flow_items", "worker_session_id", "SET NULL"),
    ("worker_sessions", "parent_session_id", "NO ACTION"),
    ("worker_sessions", "requester_session_id", "NO ACTION"),
];

fn unregistered_cascading_children(
    references: &[ReferenceToWorkerSessions],
) -> Vec<&ReferenceToWorkerSessions> {
    references
        .iter()
        .filter(|reference| {
            reference.on_delete.eq_ignore_ascii_case("cascade")
                && !EXPECTED_REFERENCES.iter().any(|(table, column, action)| {
                    reference.from_table == *table
                        && reference.from_column == *column
                        && reference.on_delete.eq_ignore_ascii_case(action)
                })
        })
        .collect()
}

/// Counter-fixture: both halves re-checked through the same functions, against a head schema plus one violation each.
#[tokio::test]
async fn the_ratchet_sees_a_cascade_and_a_trigger_when_one_exists() {
    let pool = head_pool().await;
    assert!(
        unregistered_cascading_children(&references_to_worker_sessions(&pool).await).is_empty(),
        "premise: the real schema has no unregistered cascading children"
    );

    sqlx::query(
        r#"CREATE TABLE cascade_probe (
             id TEXT PRIMARY KEY,
             session_id TEXT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE
           )"#,
    )
    .execute(&pool)
    .await
    .expect("create the violating table");
    sqlx::query(
        r#"CREATE TRIGGER trigger_probe AFTER DELETE ON cascade_probe
           BEGIN
             DELETE FROM worker_sessions WHERE id = OLD.session_id;
           END"#,
    )
    .execute(&pool)
    .await
    .expect("create the violating trigger");

    let references = references_to_worker_sessions(&pool).await;
    let cascading = unregistered_cascading_children(&references);
    assert_eq!(
        cascading.len(),
        1,
        "the foreign-key half must SEE a cascade that exists: {references:#?}"
    );
    assert_eq!(cascading[0].from_table, "cascade_probe");
    assert_eq!(cascading[0].from_column, "session_id");

    assert_eq!(
        triggers_deleting_from_worker_sessions(&pool).await,
        vec!["trigger_probe".to_string()],
        "the trigger half must SEE a trigger that deletes from the table"
    );
}
