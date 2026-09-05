//! #1449 — the set of ways a `worker_sessions` row can disappear, asked of
//! SQLite rather than re-derived from migration text.
//!
//! # Why this is a ratchet and not a one-off scan
//!
//! The planner harness's run loop refuses to issue a turn once its own row has
//! left the active set (`runtime_is_still_the_live_carrier`). That check reads a
//! single row by id, and it has to decide what `None` means. Today it treats
//! `None` as "still mine", because the only way to get `None` is that the row
//! was deleted, and every deleting path shuts the harness down first.
//!
//! **That reasoning is a statement about the schema, and schemas drift.** Add
//! one `ON DELETE CASCADE` pointing at `worker_sessions` — from any table, in
//! any future migration — and rows start vanishing under live handles with
//! nobody's shutdown involved. A one-off scan proves "no cascade today"; this
//! proves "the next migration that adds one goes red here".
//!
//! It matters more, not less, if the check becomes fail-CLOSED: then a vanished
//! row does not let a runtime speak when it should not, it silently stops one
//! that should, forever. Either way the row-disappearance set is the load-bearing
//! fact, and it is exactly the kind that rots quietly.
//!
//! # What is authoritative here and what is not, stated rather than blurred
//!
//! * **Foreign keys: authoritative.** `PRAGMA foreign_key_list` is SQLite's own
//!   answer about its own schema. The repository has burned rounds on checkers
//!   that re-derived from source text what an installed tool already knew; this
//!   does not repeat that.
//! * **Triggers: textual, and said so.** SQLite exposes no pragma for what a
//!   trigger's body writes to, so the trigger half matches `sqlite_master.sql`.
//!   A trigger that deleted from the table through a view or an alias would slip
//!   past it. Recorded as the known limit of this gate rather than papered over.
//!
//! Both halves run through the SAME two functions in the positive and the
//! negative case below, so the negative fixture proves the check that actually
//! guards the tree, not a copy of it.

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
        // The table name cannot be bound — PRAGMA takes an identifier, not a
        // value — so it is quoted. It comes from `sqlite_master`, not from a
        // caller.
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

/// Every trigger whose body deletes from `worker_sessions`.
///
/// Textual, for the reason in the module header: SQLite has no pragma for a
/// trigger's write targets.
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
    // `replay_to_head` runs `MIGRATOR` — the production chain, embedded by
    // calm-truth — exactly the way production boot runs it.
    replay_to_head(&pool).await;
    pool
}

/// THE ratchet. If this fails, a migration widened the set of ways a
/// `worker_sessions` row can vanish, and `runtime_is_still_the_live_carrier`'s
/// treatment of a missing row has to be re-decided before the expectation below
/// is updated.
#[tokio::test]
async fn nothing_in_the_schema_deletes_a_worker_sessions_row_behind_its_owner() {
    let pool = head_pool().await;

    let references = references_to_worker_sessions(&pool).await;
    let cascading: Vec<&ReferenceToWorkerSessions> = references
        .iter()
        .filter(|r| r.on_delete.eq_ignore_ascii_case("cascade"))
        .collect();
    assert!(
        cascading.is_empty(),
        "a foreign key now cascades deletes onto worker_sessions, so rows can vanish under a \
         live planner harness with no shutdown involved: {cascading:#?}"
    );

    let triggers = triggers_deleting_from_worker_sessions(&pool).await;
    assert!(
        triggers.is_empty(),
        "a trigger now deletes from worker_sessions: {triggers:#?}"
    );

    // The full inventory, frozen. Not only "nothing cascades": a reference that
    // changes its action, or a new one appearing at all, is a change to the
    // reasoning and has to be looked at.
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

/// Every foreign key in the head schema that points at `worker_sessions`, with
/// the action SQLite reports for it. Read off `PRAGMA foreign_key_list`, not off
/// the migrations.
///
/// None of them is `CASCADE`, and the two directions are worth keeping straight:
/// `cards.session_id` and `worker_flow_items.worker_session_id` are `SET NULL`,
/// which fires when a `worker_sessions` row is deleted and clears the pointer to
/// it — that is a consequence of the delete, not a cause. Nothing here can
/// remove a `worker_sessions` row as a side effect of deleting something else.
const EXPECTED_REFERENCES: &[(&str, &str, &str)] = &[
    ("cards", "session_id", "SET NULL"),
    ("tracks", "root_session_id", "NO ACTION"),
    ("worker_flow_items", "worker_session_id", "SET NULL"),
    ("worker_sessions", "parent_session_id", "NO ACTION"),
    ("worker_sessions", "requester_session_id", "NO ACTION"),
];

/// The counter-fixture, and the reason this file is a gate rather than a
/// decoration: a gate that has only ever been observed green proves nothing
/// about what it would do when the invariant breaks.
///
/// Both halves are re-checked through the SAME functions the test above calls,
/// against a database that is a real head schema plus one violation each.
#[tokio::test]
async fn the_ratchet_sees_a_cascade_and_a_trigger_when_one_exists() {
    let pool = head_pool().await;
    assert!(
        references_to_worker_sessions(&pool)
            .await
            .iter()
            .all(|r| !r.on_delete.eq_ignore_ascii_case("cascade")),
        "premise: the real schema has no cascade to begin with"
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
    let cascading: Vec<&ReferenceToWorkerSessions> = references
        .iter()
        .filter(|r| r.on_delete.eq_ignore_ascii_case("cascade"))
        .collect();
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
