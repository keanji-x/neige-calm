//! The claim-vs-completion interleaving that deadlocked under shared-cache sqlite (a DEFERRED claim tx held R(operations)
//! while the completion tx held the writer slot), exercised on the REAL operation-repo paths. Writing transactions always BEGIN IMMEDIATE.

use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use super::tests::parked_operation;
use super::*;

/// Bound for joins that must complete promptly once unblocked.
const STALL_BOUND: Duration = Duration::from_secs(30);

/// Dispatch to the connection's worker thread + step + unlock_notify registration is sub-millisecond, so 300 ms pins the park order.
const PARK_GRACE: Duration = Duration::from_millis(300);

#[tokio::test]
async fn claim_drive_batch_vs_parked_completion_no_deadlock() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    // Scratch table so the completion tx performs a non-`operations` write first, like production. Plain rowid, no AUTOINCREMENT, so `sqlite_sequence` stays out of the lock picture.
    sqlx::query("CREATE TABLE deadlock_repro_scratch (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();

    let repo = SqlxOperationRepo::new(pool.clone());
    let parked = parked_operation(&repo, now_ms() + 60_000).await;
    let claimable_id = repo
        .insert_operation(
            "claim-completion-race",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "track_id": "track-claim" }),
        )
        .await
        .unwrap();

    let (a_holding_tx, a_holding_rx) = oneshot::channel::<()>();
    let (go_a_tx, go_a_rx) = oneshot::channel::<()>();

    // Task A — BEGIN IMMEDIATE (writer slot), non-operations write, then the parked-completion UPDATE on `operations`.
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

    let repo_b = repo.clone();
    let (b_calling_tx, b_calling_rx) = oneshot::channel::<()>();
    let b = tokio::spawn(async move {
        b_calling_tx.send(()).unwrap();
        repo_b.claim_drive_batch(16).await
    });
    b_calling_rx.await.unwrap();
    // B is now parked at BEGIN IMMEDIATE holding nothing.
    sleep(PARK_GRACE).await;
    go_a_tx.send(()).unwrap();

    let a_out = timeout(STALL_BOUND, a)
        .await
        .expect("completion side must not stall")
        .unwrap();
    let b_out = timeout(STALL_BOUND, b)
        .await
        .expect("claim side must not stall")
        .unwrap();

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
    // The claim tx could only BEGIN once A's tx concluded, so it sees exactly the pending op.
    assert_eq!(claimed.len(), 1, "claimed: {claimed:?}");
    assert_eq!(claimed[0].id, claimable_id);
    let resolved = repo.get_operation(&parked.id).await.unwrap().unwrap();
    assert_eq!(resolved.phase, Phase::Succeeded);
}
