//! #1428 — migration 0093's fence: an `operations` row carrying an
//! `idempotency_key` cannot be deleted.
//!
//! **Why a database trigger and not an allowlist constant.** The house
//! precedent for "a retention pass must not eat a row somebody reads as
//! permanent" is `events_prune`'s exact-kind allowlist plus
//! `first_message_dedup_kind_is_never_prunable`. That shape works there because
//! a pruner already exists and the allowlist is the only door into it. There is
//! no `operations` pruner, so a constant would be a door nobody has to walk
//! through — decoration until the next author chooses to read it. The trigger
//! does not depend on their cooperation.
//!
//! What the trigger protects is traced end to end in
//! `docs/design-1428-idempotency-retention.md` §3.1: reaping a *succeeded*
//! keyed row makes the next byte-identical replay deliver the user's first
//! message a second time, because `OperationRuntime::submit`'s
//! `find_by_idempotency_key` short-circuit is the only thing standing between a
//! retry and a re-run.

use super::SqlxRepo;

async fn repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory repo")
}

/// Insert one `operations` row. `idempotency_key` is the only input, because
/// it is the only column the fence reads.
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

/// The fence itself. A keyed row is the `submit` dedup wall, so the database
/// refuses to let anything delete it — a future reaper, a hand-run statement in
/// a production shell, or a test helper that "just wipes the table".
#[tokio::test]
async fn a_keyed_operations_row_cannot_be_deleted() {
    let repo = repo().await;
    insert_operation(&repo, "keyed", Some("some-idempotency-key")).await;

    let refused = sqlx::query("DELETE FROM operations WHERE id = 'keyed'")
        .execute(repo.pool())
        .await
        .expect_err("deleting a keyed operations row must be refused");

    // The message is the whole point of the fence: whoever trips this in five
    // months gets the reason and the legitimate alternative, not just a
    // constraint code.
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

/// The other half of the criterion, and the half that keeps this a fence rather
/// than a wall: a NULL `idempotency_key` can never be found by
/// `find_by_idempotency_key`, so such a row is not a dedup wall and a future
/// retention pass may delete it with **no migration**.
///
/// Mutation that must redden exactly this test: delete the `WHEN OLD.
/// idempotency_key IS NOT NULL` clause from migration 0093.
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

/// The property that makes this a fence and not a tripwire on a spelling, and
/// the reason the trigger must not be "optimized away" later on the grounds
/// that no caller deletes today.
///
/// SQLite's truncate optimization would normally make a `DELETE` with no
/// `WHERE` skip row processing entirely — and therefore skip the trigger. The
/// presence of a row trigger on the table disables that optimization, so the
/// bare statement fires per row and aborts. A reaper cannot evade the fence by
/// omitting its `WHERE`, by using a query builder, or by capitalizing
/// differently.
#[tokio::test]
async fn a_bare_delete_from_operations_also_aborts() {
    let repo = repo().await;
    insert_operation(&repo, "keyed", Some("some-idempotency-key")).await;
    insert_operation(&repo, "unkeyed", None).await;

    sqlx::query("DELETE FROM operations")
        .execute(repo.pool())
        .await
        .expect_err("a bare DELETE FROM operations must abort on the keyed row");

    // Atomic: the statement is one implicit transaction, so the unkeyed row it
    // had already reached is rolled back with it. Nothing is half-reaped.
    assert_eq!(
        surviving_ids(&repo).await,
        vec!["keyed".to_string(), "unkeyed".to_string()],
        "the aborted bare delete must roll back entirely"
    );
}

/// The anti-rebuild guard.
///
/// SQLite drops a trigger silently along with its table and fires no delete
/// trigger on `DROP TABLE`, so a future migration that rebuilds `operations`
/// the way 0042 did would remove this fence with **no other failing test**.
/// This one goes red instead, and its message says what was lost rather than
/// merely that an object is missing.
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
