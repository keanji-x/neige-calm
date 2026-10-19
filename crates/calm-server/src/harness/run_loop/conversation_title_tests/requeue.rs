//! Ordinary issuance uses the same snapshot/projection handoff as dropped steers.
use super::*;
use crate::harness::run_loop::input_inspection::InputInspection;

async fn batch() -> (Fixture, QueueEntryId, QueueEntryId) {
    let f = Fixture::new().await;
    let entries = ["Original batch title", "Second batch entry"]
        .map(|text| QueueEntry::user_message(text.into(), None, vec![]));
    let first = entries[0].id().unwrap().clone();
    let second = entries[1].id().unwrap().clone();
    f.enqueue(entries.into()).await;
    f.daemon.fail_turn_start_for_test();
    (f, first, second)
}

async fn title(f: &Fixture) -> Option<String> {
    let card = f.harness.inner.card_id.to_string();
    crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
        .await
        .unwrap()[&card]
        .clone()
}

async fn reject_delete(f: &Fixture) {
    f.reject_projection_retirement().await;
}

async fn allow_delete(f: &Fixture) {
    sqlx::query("DROP TRIGGER refuse_projection_cleanup")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
}

async fn edit_delete(f: &Fixture, first: &QueueEntryId, second: &QueueEntryId) {
    f.harness
        .mutate_pending_entry(
            QueueMutation::Edit {
                entry_id: first.clone(),
                text: "Edited batch title".into(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        title(f).await.as_deref(),
        Some("Edited batch title"),
        "a returned queue edit must not be hidden by the old batch projection"
    );
    f.harness
        .mutate_pending_entry(
            QueueMutation::Delete {
                entry_id: first.clone(),
                if_entry_rev: 1,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(title(f).await.as_deref(), Some("Second batch entry"));
    f.harness
        .mutate_pending_entry(
            QueueMutation::Delete {
                entry_id: second.clone(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(title(f).await, None);
}

#[tokio::test]
async fn conversation_title_requeue_delete_failure_retries_batch_before_edit_delete() {
    let (f, first, second) = batch().await;
    reject_delete(&f).await;
    let issued = maybe_issue_turn(&f.harness.inner).await;
    let stored = f.stored().await;
    assert_eq!(
        f.harness.inner.projection_client_id.lock().await.as_ref(),
        Some(&first)
    );
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        f.harness.inspect_input(second.clone()).await.unwrap(),
        InputInspection::Queued { rev: 0, .. }
    ));
    allow_delete(&f).await;
    // The queue mutation is itself a production retry of the pending transfer.
    edit_delete(&f, &first, &second).await;
    assert!(
        issued.is_err(),
        "a failed retirement must fail its checkpoint"
    );
    assert_eq!(
        stored.phase,
        HarnessPhaseTag::IssuingTurn,
        "failed retirement must roll back the snapshot too"
    );
    assert_eq!(stored.pending_entries().len(), 2);
    assert!(f.harness.inner.restored_projections.lock().await.is_empty());
}

#[tokio::test]
async fn conversation_title_requeue_normal_batch_retires_only_its_projection() {
    let (f, first, second) = batch().await;
    maybe_issue_turn(&f.harness.inner).await.unwrap();
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_none()
    );
    edit_delete(&f, &first, &second).await;
}

#[tokio::test]
async fn conversation_title_requeue_cancel_retry_keeps_complete_retirement() {
    let (f, first, second) = batch().await;
    reject_delete(&f).await;
    assert!(maybe_issue_turn(&f.harness.inner).await.is_err());
    allow_delete(&f).await;
    let mut blocker = crate::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET updated_at_ms=updated_at_ms")
        .execute(&mut **blocker)
        .await
        .unwrap();
    let inner = f.harness.inner.clone();
    let mut retry = tokio::spawn(async move { persist_issuance_outcome(&inner).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut retry)
            .await
            .is_err()
    );
    retry.abort();
    assert!(retry.await.unwrap_err().is_cancelled());
    blocker.rollback().await.unwrap();
    assert_eq!(
        *f.harness.inner.restored_projections.lock().await,
        vec![first.clone()],
        "one batch key owns both entries, not just its first entry"
    );
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_some()
    );
    persist_issuance_outcome(&f.harness.inner).await.unwrap();
    edit_delete(&f, &first, &second).await;
}

#[tokio::test]
async fn conversation_title_requeue_retired_writer_rolls_back_and_retries() {
    let (f, first, _) = batch().await;
    reject_delete(&f).await;
    assert!(maybe_issue_turn(&f.harness.inner).await.is_err());
    // The issuance fence may have retired its row while the backend was answering.
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?")
        .bind(&f.harness.inner.worker_session_id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
    assert!(persist_issuance_outcome(&f.harness.inner).await.is_err());
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(
        *f.harness.inner.restored_projections.lock().await,
        vec![first.clone()]
    );
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_some()
    );
    allow_delete(&f).await;
    persist_issuance_outcome(&f.harness.inner).await.unwrap();
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::TurnCompleted);
    assert!(f.harness.inner.restored_projections.lock().await.is_empty());
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn conversation_title_requeue_unwritten_guard_retains_source_and_retry() {
    let (f, first, second) = batch().await;
    reject_delete(&f).await;
    assert!(maybe_issue_turn(&f.harness.inner).await.is_err());
    allow_delete(&f).await;
    // Neither active (shutdown) nor retired (still active in storage) writer can match.
    f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
    persist_issuance_outcome(&f.harness.inner).await.unwrap();
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(
        *f.harness.inner.restored_projections.lock().await,
        vec![first.clone()]
    );
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_some()
    );
    f.harness.inner.shutting_down.store(false, Ordering::SeqCst);
    persist_issuance_outcome(&f.harness.inner).await.unwrap();
    let successor =
        super::super::input_inspection_recovery_tests::transfer_before_issuance(&f).await;
    assert!(matches!(
        successor.inspect_input(second.clone()).await.unwrap(),
        InputInspection::Queued { rev: 0, .. }
    ));
    successor
        .mutate_pending_entry(
            QueueMutation::Edit {
                entry_id: first.clone(),
                text: "Recovered title".into(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(title(&f).await.as_deref(), Some("Recovered title"));
    for (entry_id, if_entry_rev) in [(first, 1), (second, 0)] {
        successor
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id,
                    if_entry_rev,
                },
                ActorId::User,
            )
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(title(&f).await, None);
}

#[tokio::test]
async fn conversation_title_requeue_snapshot_failure_keeps_projection_and_retries() {
    let (f, first, second) = batch().await;
    // Allow pre-drain persistence; reject the returned queue's commit, not the projection.
    sqlx::query(
        "CREATE TRIGGER reject_requeue_snapshot BEFORE UPDATE ON worker_sessions
        WHEN json_extract(NEW.handle_state_json, '$.phase') = 'turn_completed'
        BEGIN SELECT RAISE(ABORT, 'test requeue snapshot failure'); END",
    )
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(maybe_issue_turn(&f.harness.inner).await.is_err());
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), first.as_str())
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("DROP TRIGGER reject_requeue_snapshot")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    edit_delete(&f, &first, &second).await;
}

#[tokio::test]
async fn conversation_title_requeue_shutdown_preserves_retry_and_recovery_edits() {
    for failed_shutdown in [false, true] {
        let (f, first, second) = batch().await;
        reject_delete(&f).await;
        assert!(maybe_issue_turn(&f.harness.inner).await.is_err());
        if failed_shutdown {
            assert!(f.harness.shutdown().await.is_err());
            assert_eq!(
                *f.harness.inner.restored_projections.lock().await,
                vec![first.clone()]
            );
            assert_eq!(f.stored().await.phase, HarnessPhaseTag::IssuingTurn);
        }
        allow_delete(&f).await;
        // No manual checkpoint: exercise the actual shutdown/start handoff after failure.
        let successor =
            super::super::input_inspection_recovery_tests::transfer_before_issuance(&f).await;
        assert!(f.harness.inner.restored_projections.lock().await.is_empty());
        successor
            .mutate_pending_entry(
                QueueMutation::Edit {
                    entry_id: first.clone(),
                    text: "Recovered after shutdown".into(),
                    if_entry_rev: 0,
                },
                ActorId::User,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(title(&f).await.as_deref(), Some("Recovered after shutdown"));
        for (entry_id, if_entry_rev) in [(first, 1), (second, 0)] {
            successor
                .mutate_pending_entry(
                    QueueMutation::Delete {
                        entry_id,
                        if_entry_rev,
                    },
                    ActorId::User,
                )
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(title(&f).await, None);
    }
}
