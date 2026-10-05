//! #2139 R1: the worker-supplied commit message — its one validator, the 0145 column's CHECK and
//! trigger, the argv a stored message builds, and a row written before the migration.

use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

use super::*;
use crate::git_candidate::commit_message::COMMIT_MESSAGE_MAX_BYTES;

/// A message shaped like the #2129 worker's: conventional subject, body, and ownership lines the
/// kernel treats as plain text.
const OWNERSHIP_MESSAGE: &str = "fix(forge): include exact CI evidence in planner wakes\n\n\
     Closes #2129\n\n\
     OWNERSHIP-CHANGE: fe/core/api/schemas.ts — regenerated wire schema (#2129)\n";

fn worker(text: &str) -> DeliveryMessage {
    DeliveryMessage::Worker(CommitMessage::parse(text).unwrap())
}

#[test]
fn commit_message_parse_boundaries() {
    assert_eq!(COMMIT_MESSAGE_MAX_BYTES, 16_384);
    let at_limit = "a".repeat(16_384);
    assert_eq!(CommitMessage::parse(&at_limit).unwrap().as_str(), at_limit);
    assert_eq!(
        CommitMessage::parse(&"a".repeat(16_385)),
        Err("commit_message is 16385 bytes; the limit is 16384".to_string())
    );
    // The limit counts UTF-8 bytes, not characters.
    let wide_at_limit = "é".repeat(8_192);
    assert!(CommitMessage::parse(&wide_at_limit).is_ok());
    assert_eq!(
        CommitMessage::parse(&format!("{wide_at_limit}a")),
        Err("commit_message is 16385 bytes; the limit is 16384".to_string())
    );

    for refused in [
        ("subject\0body", "commit_message has a NUL byte"),
        ("\0", "commit_message has a NUL byte"),
        (
            "subject \u{1b}[31mred",
            "commit_message has the control character U+001B",
        ),
        (
            "subject\u{7f}",
            "commit_message has the control character U+007F",
        ),
        ("", "commit_message is empty"),
        (" \n\t\r\n ", "commit_message is empty"),
    ] {
        assert_eq!(
            CommitMessage::parse(refused.0),
            Err(refused.1.to_string()),
            "{:?}",
            refused.0
        );
    }

    // Tab, LF and CR, trailing spaces and multi-byte text are carried verbatim, not normalised.
    for accepted in [
        "subject\tx\r\n\r\nbody  \n\n",
        "fix: ünïcödé ✓ 日本語",
        OWNERSHIP_MESSAGE,
    ] {
        assert_eq!(CommitMessage::parse(accepted).unwrap().as_str(), accepted);
    }
}

/// The text every row without a worker message commits with, byte for byte as before #2139: the
/// 4140 rows rely on it.
#[test]
fn kernel_message_is_byte_equal_to_the_pre_2139_text() {
    let row = delivery_row(None);
    assert_eq!(row.commit_message, DeliveryMessage::Kernel);
    for (outcome, text) in [
        (
            AttemptOutcome::Completed,
            "neige: attempt attempt-1 completed (delivery dlv1)",
        ),
        (
            AttemptOutcome::Failed,
            "neige: attempt attempt-1 failed (delivery dlv1)",
        ),
        (
            AttemptOutcome::Canceled,
            "neige: attempt attempt-1 canceled (delivery dlv1)",
        ),
        (
            AttemptOutcome::SpawnFailed,
            "neige: attempt attempt-1 spawn-failed (delivery dlv1)",
        ),
        (
            AttemptOutcome::Interrupted,
            "neige: attempt attempt-1 interrupted (delivery dlv1)",
        ),
    ] {
        assert_eq!(delivery_message(&row, outcome), text);
    }
}

/// A stored worker message is read back as written and is `$1` of the delivery argv, verbatim.
#[tokio::test]
async fn delivery_message_is_the_stored_worker_message() {
    let fx = db_fixture().await;
    let inserted = fx
        .insert_delivery_with("attempt-1", worker(OWNERSHIP_MESSAGE))
        .await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let stored = delivery_by_id_tx(&mut tx, &inserted.delivery_id)
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(stored, inserted);
    assert_eq!(stored.commit_message, worker(OWNERSHIP_MESSAGE));

    let branch = track_branch_for(&fx.track_id).unwrap();
    let payload = forge_payload_for(&stored, &fx.lease, &branch).unwrap();
    assert_eq!(payload.argv[3], "sh");
    assert_eq!(payload.argv[4], OWNERSHIP_MESSAGE);
}

/// The 0145 CHECK admits a message only on a completed row (a NULL outcome included) and only
/// within 1..=16384 bytes; the trigger makes it immutable while both settlement UPDATEs, which
/// never name it, still settle a messaged row.
#[tokio::test]
async fn commit_message_is_completed_only_and_immutable() {
    let fx = db_fixture().await;
    let insert = |index: usize, outcome: Option<&'static str>, message: String| {
        sqlx::query(
            "INSERT INTO task_git_deliveries (delivery_id, track_id, producer_attempt_id, \
             card_id, lease_id, ordinal, operation_key, forge_idempotency_key, outcome, \
             commit_message, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, 1)",
        )
        .bind(format!("raw-{index}"))
        .bind(fx.track_id.clone())
        .bind(format!("raw-attempt-{index}"))
        .bind(fx.card_id.clone())
        .bind(fx.lease.lease_id.clone())
        .bind(format!("raw-op-{index}"))
        .bind(format!("raw-idem-{index}"))
        .bind(outcome)
        .bind(message)
        .execute(fx.repo.pool())
    };
    let mut accepted = Vec::new();
    for (index, (outcome, message)) in [
        (Some("completed"), "m".to_string()),
        (Some("completed"), "m".repeat(16_384)),
        (Some("completed"), "m".repeat(16_385)),
        (Some("completed"), String::new()),
        (Some("failed"), "m".to_string()),
        (Some("canceled"), "m".to_string()),
        (Some("spawn-failed"), "m".to_string()),
        (Some("interrupted"), "m".to_string()),
        (None, "m".to_string()),
    ]
    .into_iter()
    .enumerate()
    {
        let len = message.len();
        match insert(index, outcome, message).await {
            Ok(_) => accepted.push((outcome, len)),
            Err(error) => assert!(
                error.to_string().contains("CHECK constraint failed"),
                "({outcome:?}, {len}) refused by something other than the CHECK: {error}"
            ),
        }
    }
    assert_eq!(
        accepted,
        vec![(Some("completed"), 1), (Some("completed"), 16_384)]
    );

    // The row writer itself cannot pair a worker message with another outcome.
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let refused = insert_initial_delivery_tx(
        &mut tx,
        "attempt-failed",
        &fx.lease,
        AttemptOutcome::Failed,
        worker("m"),
        1,
    )
    .await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("CHECK constraint failed")),
        "{refused:?}"
    );
    tx.rollback().await.unwrap();

    // Immutable: a settlement UPDATE that also rewrites the message passes the 0113 trigger (it
    // names its columns) and is stopped by 0145's; a bare rewrite is refused too.
    let candidate_row = fx
        .insert_delivery_with("attempt-candidate", worker("first"))
        .await;
    let rewrite = sqlx::query(
        "UPDATE task_git_deliveries SET settlement = 'candidate', settled_event_id = 1, \
         wake_reason = 'ungated_candidate', commit_message = 'second' WHERE delivery_id = ?1",
    )
    .bind(&candidate_row.delivery_id)
    .execute(fx.repo.pool())
    .await
    .unwrap_err();
    assert!(
        rewrite
            .to_string()
            .contains("git delivery commit message is immutable"),
        "{rewrite}"
    );
    for value in [Some("second"), None] {
        let bare = sqlx::query(
            "UPDATE task_git_deliveries SET commit_message = ?2 WHERE delivery_id = ?1",
        )
        .bind(&candidate_row.delivery_id)
        .bind(value)
        .execute(fx.repo.pool())
        .await
        .unwrap_err();
        assert!(bare.to_string().contains("immutable"), "{bare}");
    }

    // Both settlement UPDATEs settle a messaged row and leave the message as written.
    let failed_row = fx
        .insert_delivery_with("attempt-failed-settle", worker("failed settle"))
        .await;
    let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    let candidate = fx.candidate_for(&candidate_row, &"c".repeat(40));
    assert_eq!(
        settle_candidate_tx(
            &mut tx,
            &candidate_row.delivery_id,
            1,
            DeliveryWakeReason::UngatedCandidate,
            &candidate,
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        settle_failed_tx(
            &mut tx,
            &failed_row.delivery_id,
            2,
            DeliveryFailureCode::CommitFailed,
            "hook",
            true,
        )
        .await
        .unwrap(),
        1
    );
    let candidate_after = delivery_by_id_tx(&mut tx, &candidate_row.delivery_id)
        .await
        .unwrap()
        .unwrap();
    let failed_after = delivery_by_id_tx(&mut tx, &failed_row.delivery_id)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert!(candidate_after.settlement.is_some());
    assert_eq!(candidate_after.commit_message, worker("first"));
    assert!(failed_after.settlement.is_some());
    assert_eq!(failed_after.commit_message, worker("failed settle"));
}

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// An unsettled completed row written at the 4140 production schema (migrations through 0140,
/// no forge Operation yet) reads back as `Kernel` after the upgrade and rebuilds the argv the
/// pre-#2139 builder produced, byte for byte.
#[tokio::test]
async fn pre_2139_unsettled_row_rebuilds_identical_argv() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    migrator_through(140)
        .run(&pool)
        .await
        .expect("apply migrations through 0140 (4140 production)");
    sqlx::raw_sql(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at) \
           VALUES ('area-4140', 'a', '#000', 0, 1, 1); \
         INSERT INTO tracks (id, area_id, title, sort, created_at, updated_at) \
           VALUES ('track-4140', 'area-4140', 't', 0, 1, 1); \
         INSERT INTO workspace_leases (lease_id, card_id, track_id, path, state, lease_owner, \
           created_at_ms, updated_at_ms, base_sha, base_source, canonical_path, git_common_dir, \
           delivery_policy) \
           VALUES ('lease-4140', 'card-4140', 'track-4140', '/repo/.claude/worktrees/track-4140', \
           'released', 'op-4140', 1, 1, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'head', \
           '/repo/.claude/worktrees/track-4140', '/repo/.git', 'kernel'); \
         INSERT INTO task_git_deliveries (delivery_id, track_id, producer_attempt_id, card_id, \
           lease_id, ordinal, operation_key, forge_idempotency_key, outcome, created_at_ms) \
           VALUES ('dlv-4140', 'track-4140', 'attempt-4140', 'card-4140', 'lease-4140', 1, \
           'op-key-4140', 'idem-4140', 'completed', 1);",
    )
    .execute(&pool)
    .await
    .expect("seed a 4140-shaped unsettled delivery");

    calm_truth::MIGRATOR
        .run(&pool)
        .await
        .expect("upgrade to head");

    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let row = delivery_by_id_tx(&mut tx, "dlv-4140")
        .await
        .unwrap()
        .unwrap();
    let lease = lease_for_delivery_tx(&mut tx, &row).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(row.commit_message, DeliveryMessage::Kernel);
    assert!(row.settlement.is_none());

    let branch = track_branch_for("track-4140").unwrap();
    let payload = forge_payload_for(&row, &lease, &branch).unwrap();
    assert_eq!(
        payload.argv,
        delivery_argv(
            "neige: attempt attempt-4140 completed (delivery dlv-4140)",
            &branch,
            "refs/neige/candidates/track-4140/card-4140/dlv-4140",
            &"b".repeat(40),
            "/repo/.claude/worktrees/track-4140",
            "/repo/.git",
        )
    );
}
