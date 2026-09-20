//! Migration 0093's fence: an `operations` row carrying an `idempotency_key`
//! cannot be deleted.

use super::SqlxRepo;

async fn repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory repo")
}

async fn insert_operation(repo: &SqlxRepo, id: &str, idempotency_key: Option<&str>) {
    sqlx::query(
        r#"INSERT INTO operations (
             id, operation_key, kind, idempotency_key, payload_hash,
             target_type, target_json, payload_json, phase,
             created_at_ms, updated_at_ms
           ) VALUES (?1, ?1, 'planner-harness-start', ?2, 'hash', 'track', '{}', '{}',
                     'succeeded', 0, 0)"#,
    )
    .bind(id)
    .bind(idempotency_key)
    .execute(repo.pool())
    .await
    .expect("insert operation");
}

async fn surviving_ids(repo: &SqlxRepo) -> Vec<String> {
    sqlx::query_scalar("SELECT id FROM operations ORDER BY id")
        .fetch_all(repo.pool())
        .await
        .expect("read operations")
}

#[tokio::test]
async fn a_keyed_operations_row_cannot_be_deleted() {
    let repo = repo().await;
    insert_operation(&repo, "keyed", Some("some-idempotency-key")).await;

    let refused = sqlx::query("DELETE FROM operations WHERE id = 'keyed'")
        .execute(repo.pool())
        .await
        .expect_err("deleting a keyed operations row must be refused");

    let message = refused.to_string();
    assert!(
        message.contains("submit() dedup wall"),
        "the abort must say WHY the row is permanent, got: {message}"
    );
    assert!(
        message.contains("idempotency_key IS NULL"),
        "the abort must name what a retention pass MAY still delete, got: {message}"
    );
    assert!(
        message.contains("docs/design-1428-idempotency-retention.md"),
        "the abort must point at the reasoning, got: {message}"
    );
    assert_eq!(
        surviving_ids(&repo).await,
        vec!["keyed".to_string()],
        "the refused delete must leave the row in place"
    );
}

#[tokio::test]
async fn an_unkeyed_operations_row_is_still_deletable() {
    let repo = repo().await;
    insert_operation(&repo, "keyed", Some("some-idempotency-key")).await;
    insert_operation(&repo, "unkeyed", None).await;

    sqlx::query("DELETE FROM operations WHERE idempotency_key IS NULL")
        .execute(repo.pool())
        .await
        .expect("an unkeyed operations row must remain deletable");

    assert_eq!(
        surviving_ids(&repo).await,
        vec!["keyed".to_string()],
        "the unkeyed row goes, the keyed one stays"
    );
}

/// A row trigger disables SQLite's truncate optimization, so a bare DELETE still fires per row.
#[tokio::test]
async fn a_bare_delete_from_operations_also_aborts() {
    let repo = repo().await;
    insert_operation(&repo, "keyed", Some("some-idempotency-key")).await;
    insert_operation(&repo, "unkeyed", None).await;

    sqlx::query("DELETE FROM operations")
        .execute(repo.pool())
        .await
        .expect_err("a bare DELETE FROM operations must abort on the keyed row");

    assert_eq!(
        surviving_ids(&repo).await,
        vec!["keyed".to_string(), "unkeyed".to_string()],
        "the aborted bare delete must roll back entirely"
    );
}

/// SQLite fires no delete trigger on `DROP TABLE`, so a migration that rebuilds
/// `operations` silently loses the fence.
#[tokio::test]
async fn head_schema_has_the_keyed_operations_fence() {
    let repo = repo().await;
    let trigger: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'trigger' AND name = 'operations_keyed_rows_are_permanent'",
    )
    .fetch_optional(repo.pool())
    .await
    .expect("read sqlite_master");

    assert_eq!(
        trigger.as_deref(),
        Some("operations_keyed_rows_are_permanent"),
        "migration 0093's fence is missing from the head schema. If you rebuilt the `operations` \
         table (the rename → create → copy → drop shape 0042 used), SQLite dropped the trigger \
         with the old table and your rebuild must recreate it. Without it, an `operations` \
         retention pass can delete a keyed row, and the next byte-identical retry re-runs the \
         operation and delivers its message a second time — see \
         docs/design-1428-idempotency-retention.md §3."
    );
}
