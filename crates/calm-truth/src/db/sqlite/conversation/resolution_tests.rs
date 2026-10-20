//! Human Keep uses the production transaction and retained checkpoint, not a reducer copy.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::held_projection_tests::held;
use super::*;
use calm_types::conversation::*;
use serde_json::json;

fn request(worker: &str, attempt: &InputAttempt) -> KeepConversationInputRequest {
    KeepConversationInputRequest {
        carrier_id: worker.into(),
        submission_id: attempt.submission_id.to_string(),
        attempt_id: attempt.attempt_id.to_string(),
        revision: "1".into(),
    }
}

async fn keep(
    repo: &SqlxRepo,
    card: &str,
    human: &str,
    key: &str,
    args: KeepConversationInputRequest,
) -> crate::error::Result<KeepConversationInputResult> {
    let mut tx = begin_immediate_tx(repo.db()).await?;
    let result = keep_conversation_input_tx(&mut tx, card, human, key, &args).await?;
    tx.commit().await?;
    Ok(result)
}

#[tokio::test]
async fn canonical_keep_preserves_delivery_and_original_end_target() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut before = current_input_fact_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let mut correlation = before.checkpoint().attempts[0].correlation.clone();
    correlation.native_turn_id = Some("original-native-turn".into());
    record_input_attempt_fact_tx(
        &mut tx,
        &mut before,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation,
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    let original = original_input_execution_tx(&mut tx, &before).await.unwrap();
    tx.commit().await.unwrap();
    let mut args = request(&worker, &attempt);
    args.revision = "2".into();
    let result = keep(&repo, &card, "human", "keep", args).await.unwrap();
    assert_eq!(result.revision, "3");
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let after = current_input_fact_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    assert_eq!(
        after.checkpoint().attempts[0].delivery,
        before.checkpoint().attempts[0].delivery
    );
    assert_eq!(
        after.checkpoint().attempts[0].correlation,
        before.checkpoint().attempts[0].correlation
    );
    assert_eq!(after.checkpoint().active, before.checkpoint().active);
    assert_eq!(after.phase().unwrap(), before.phase().unwrap());
    assert_eq!(
        original_input_execution_tx(&mut tx, &after).await.unwrap(),
        original
    );
    assert_eq!(after.checkpoint().receipts.len(), 1);
}

#[tokio::test]
async fn canonical_keep_response_loss_replays_after_carrier_superseded() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("keep.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let args = request(&worker, &attempt);
    let result = keep(&repo, &card, "human", "keep", args.clone())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    drop(repo);
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        keep(&repo, &card, "human", "keep", args).await.unwrap(),
        result
    );
}

#[tokio::test]
async fn canonical_keep_key_fingerprint_includes_human_and_all_arguments() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let args = request(&worker, &attempt);
    keep(&repo, &card, "human", "keep", args.clone())
        .await
        .unwrap();
    for field in ["carrier_id", "submission_id", "attempt_id", "revision"] {
        let mut changed = serde_json::to_value(&args).unwrap();
        changed[field] = json!("different");
        assert!(matches!(
            keep(
                &repo,
                &card,
                "human",
                "keep",
                serde_json::from_value(changed).unwrap()
            )
            .await,
            Err(crate::error::CalmError::IdempotencyKeyReused(_))
        ));
    }
    assert!(matches!(
        keep(&repo, &card, "other-human", "keep", args).await,
        Err(crate::error::CalmError::IdempotencyKeyReused(_))
    ));
}

#[tokio::test]
async fn canonical_keep_racing_keys_resolves_once() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let args = request(&worker, &attempt);
    let (first, second) = tokio::join!(
        keep(&repo, &card, "human", "first", args.clone()),
        keep(&repo, &card, "human", "second", args),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(
        current_input_fact_carrier_tx(&mut tx, &card, &worker)
            .await
            .unwrap()
            .checkpoint()
            .receipts
            .len(),
        1
    );
}

#[tokio::test]
async fn canonical_keep_stale_or_overflow_writes_nothing() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    for revision in [0, u64::MAX] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let mut carrier = current_input_fact_carrier_tx(&mut tx, &card, &worker)
            .await
            .unwrap();
        carrier.checkpoint.attempts[0].revision = revision;
        super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
            .await
            .unwrap();
        let before = carrier.snapshot().clone();
        tx.commit().await.unwrap();
        let mut args = request(&worker, &attempt);
        if revision == u64::MAX {
            args.revision = revision.to_string();
        }
        assert!(keep(&repo, &card, "human", "keep", args).await.is_err());
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        assert_eq!(
            *current_input_fact_carrier_tx(&mut tx, &card, &worker)
                .await
                .unwrap()
                .snapshot(),
            before
        );
    }
}

#[tokio::test]
async fn canonical_keep_failed_and_dormant_use_same_transaction() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    for state in ["idle", "failed"] {
        let (card, worker, attempt) = held(&repo).await;
        sqlx::query("UPDATE worker_sessions SET state=?1 WHERE id=?2")
            .bind(state)
            .bind(&worker)
            .execute(repo.pool())
            .await
            .unwrap();
        assert_eq!(
            keep(&repo, &card, "human", "keep", request(&worker, &attempt))
                .await
                .unwrap()
                .revision,
            "2"
        );
    }
}

#[tokio::test]
async fn canonical_keep_runtime_frame_cannot_erase_receipt() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let before = current_input_fact_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let fence = RuntimeExecutionFence::from_carrier(&before);
    tx.commit().await.unwrap();
    keep(&repo, &card, "human", "keep", request(&worker, &attempt))
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        persist_runtime_frame_tx(&mut tx, &card, &worker, &fence, fence.header().clone())
            .await
            .is_err()
    );
    assert_eq!(
        current_input_fact_carrier_tx(&mut tx, &card, &worker)
            .await
            .unwrap()
            .checkpoint()
            .receipts
            .len(),
        1
    );
}

#[tokio::test]
async fn canonical_keep_prevents_late_non_dispatch_from_requeueing_the_input() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, worker, attempt) = held(&repo).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_fact_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    // A valid persisted pre-call marker, while its provider result is in flight.
    carrier.checkpoint.attempts[0].delivery = InputDeliveryFact::CrossingUnknown;
    carrier.checkpoint.attempts[0].revision = 0;
    super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut args = request(&worker, &attempt);
    args.revision = "0".into();
    keep(&repo, &card, "human", "keep", args).await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut carrier = current_input_fact_carrier_tx(&mut tx, &card, &worker)
        .await
        .unwrap();
    let correlation = carrier.checkpoint().attempts[0].correlation.clone();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation,
            delivery: InputDeliveryFact::NotDispatched {
                basis: "local_preflight".into(),
            },
        },
    )
    .await
    .unwrap();
    assert!(
        release_submission_tx(&mut tx, &mut carrier, &attempt)
            .await
            .is_err()
    );
    assert!(carrier.checkpoint().pending.is_empty());
    let effective:i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE submission_id=?1 AND effective=1")
        .bind(attempt.submission_id.as_str()).fetch_one(&mut **tx).await.unwrap();
    assert_eq!(effective, 1);
}
