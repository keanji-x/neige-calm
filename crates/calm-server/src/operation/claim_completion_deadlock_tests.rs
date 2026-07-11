//! #930 — the production claim-vs-completion interleaving that deadlocked
//! under shared-cache sqlite, exercised on the REAL operation-repo code
//! paths.
//!
//! The pre-fix cycle (observed in CI as the `gh.pr.merge` MCP "database is
//! deadlocked" error):
//!
//! * completion side (forge_action_adapter observer shape):
//!   `begin_immediate_tx` — holds the shared cache's single writer slot —
//!   performs a non-`operations` write, then `complete_parked_tx`'s
//!   `UPDATE operations …` needs W(operations);
//! * claim side (`claim_drive_batch`, pre-fix): DEFERRED `pool.begin()`,
//!   `SELECT id FROM operations …` takes R(operations) held to tx end,
//!   then `UPDATE operations …` parks on the writer slot.
//!
//! completion waits on claim's R(operations); claim waits on completion's
//! writer slot → wait cycle → whichever side registers its unlock_notify
//! wait second fails with plain `SQLITE_LOCKED` (6) "database is
//! deadlocked" (see `calm_truth::db::sqlite::deadlock_semantics_tests` for
//! the pinned upstream semantics). Verbatim pre-fix capture of this exact
//! test (deterministic, 10/10):
//!
//! ```text
//! side A (parked completion): completion ERR: display=database error:
//!   error returned from database: (code: 6) database is deadlocked
//! side B (claim_drive_batch): claim OK: 1 op(s)
//! ```
//!
//! POST-FIX assertion (#930 uniform rule: writing transactions always
//! BEGIN IMMEDIATE): `claim_drive_batch` now parks at BEGIN holding
//! nothing, the completion commits, the claim proceeds — both sides
//! complete without error.

use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use super::tests::parked_operation;
use super::*;

/// Bound for joins that must complete promptly once unblocked.
const STALL_BOUND: Duration = Duration::from_secs(30);

/// Grace period after a task signalled "about to issue the parking
/// statement": dispatch to the connection's worker thread + step +
/// unlock_notify registration is sub-millisecond, so 300 ms pins the park
/// order (same constant the deadlock semantics suite proved at 20/20).
const PARK_GRACE: Duration = Duration::from_millis(300);

#[tokio::test]
async fn claim_drive_batch_vs_parked_completion_no_deadlock() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    // Scratch table so the completion transaction performs a
    // non-`operations` write before touching `operations`, exactly like
    // production (decision-event append inside the completion tx). Plain
    // rowid table, no AUTOINCREMENT, so `sqlite_sequence` stays out of the
    // lock picture.
    sqlx::query("CREATE TABLE deadlock_repro_scratch (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();

    let repo = SqlxOperationRepo::new(pool.clone());
    // A parked forge-style operation whose completion observer will race
    // the claim loop…
    let parked = parked_operation(&repo, now_ms() + 60_000).await;
    // …and a claimable (pending, lease-free) operation for the claim side.
    let claimable_id = repo
        .insert_operation(
            "claim-completion-race",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-claim" }),
        )
        .await
        .unwrap();

    let (a_holding_tx, a_holding_rx) = oneshot::channel::<()>();
    let (go_a_tx, go_a_rx) = oneshot::channel::<()>();

    // Task A — the parked op's completion observer, real production shape:
    // BEGIN IMMEDIATE (writer slot), non-operations write, then the
    // parked-completion UPDATE on `operations`.
    let pool_a = pool.clone();
    let parked_id = parked.id.clone();
    let a = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool_a).await?;
        sqlx::query("INSERT INTO deadlock_repro_scratch (v) VALUES ('completion')")
            .execute(&mut *tx)
            .await
            .map_err(CalmError::from)?;
        a_holding_tx.send(()).unwrap();
        go_a_rx.await.unwrap();
        let completion = complete_parked_tx(
            &mut tx,
            &parked_id,
            &ParkedOutcome::Succeeded {
                result: json!({ "merged": true }),
            },
        )
        .await?;
        tx.commit().await.map_err(CalmError::from)?;
        Ok::<ParkedCompletion, CalmError>(completion)
    });

    // Only dispatch the claim once A holds the writer slot.
    a_holding_rx.await.unwrap();

    // Task B — the REAL claim path.
    let repo_b = repo.clone();
    let (b_calling_tx, b_calling_rx) = oneshot::channel::<()>();
    let b = tokio::spawn(async move {
        b_calling_tx.send(()).unwrap();
        repo_b.claim_drive_batch(16).await
    });
    b_calling_rx.await.unwrap();
    // Pre-fix: B's deferred SELECT has taken R(operations) and its UPDATE
    // is parked on the writer slot. Post-fix: B is parked at BEGIN
    // IMMEDIATE holding nothing.
    sleep(PARK_GRACE).await;
    // A's operations-UPDATE now closes (pre-fix) / cannot form (post-fix)
    // the cycle.
    go_a_tx.send(()).unwrap();

    let a_out = timeout(STALL_BOUND, a)
        .await
        .expect("completion side must not stall")
        .unwrap();
    let b_out = timeout(STALL_BOUND, b)
        .await
        .expect("claim side must not stall")
        .unwrap();

    // ---- POST-FIX assertion (#930) --------------------------------------
    // No wait cycle can form: both sides complete without error.
    let completion = a_out.expect(
        "parked completion must succeed: the IMMEDIATE claim tx parks at \
         BEGIN holding no locks, so no cycle exists (#930)",
    );
    assert!(
        matches!(completion, ParkedCompletion::Completed(_)),
        "parked op must complete exactly once: {completion:?}"
    );
    let claimed =
        b_out.expect("claim_drive_batch must succeed after the completion tx commits (#930)");
    // The claim tx could only BEGIN once A's tx concluded, so it sees
    // exactly the pending op — the parked op completed to 'succeeded',
    // which is not claimable.
    assert_eq!(claimed.len(), 1, "claimed: {claimed:?}");
    assert_eq!(claimed[0].id, claimable_id);
    // And the parked op really is resolved.
    let resolved = repo.get_operation(&parked.id).await.unwrap().unwrap();
    assert_eq!(resolved.phase, Phase::Succeeded);
}
