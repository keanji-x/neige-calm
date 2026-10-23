//! Restored steers can be the first visible you after a system-only turn.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::codex_appserver::Notification;
use crate::db::RepoRead;
use crate::harness::codex_events::planner_event;
use calm_truth::db::RepoOutOfDomain as _;
use serde_json::json;

async fn first_visible_steer() -> (Fixture, QueueEntryId, String) {
    let f = Fixture::new().await;
    let (_, system) = f
        .event_entry(&Event::TaskCompleted {
            notification: calm_types::event::PlannerNotification::Notify,
            idempotency_key: "title-system-turn".into(),
            result: json!({"summary":"system context"}),
            artifacts: Vec::new(),
            agent_message: None,
        })
        .await;
    f.enqueue(vec![system]).await;
    f.issue().await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 1);
    let turn_id = f.harness.inner.last_turn_id.lock().await.clone().unwrap();
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        planner_event(Notification::parse(
            "turn/started".into(),
            json!({
                "threadId":"thread-commit-wake", "turn":{"id":turn_id}
            }),
        )),
    )
    .await
    .unwrap();
    let card = f.harness.inner.card_id.to_string();
    assert_eq!(
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap()[&card],
        None
    );
    let entry = QueueEntry::user_message("First visible steer".into(), None, Vec::new());
    let id = entry.id().unwrap().clone();
    f.enqueue(vec![entry]).await;
    assert!(
        handle_steer(&f.harness.inner, &id, 0, &ActorId::Kernel)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(f.stored().await.pending_entries().is_empty());
    assert_eq!(
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap()[&card]
            .as_deref(),
        Some("First visible steer")
    );
    (f, id, turn_id)
}

#[tokio::test]
async fn conversation_title_restored_first_you_is_durable_before_projection_delete() {
    let (f, id, turn) = first_visible_steer().await;
    sqlx::query("CREATE TABLE restore_title_audit (snapshot TEXT NOT NULL)")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    // Capture the actual durable snapshot at the deletion boundary, not the final state.
    sqlx::query("CREATE TRIGGER restore_title_deleted AFTER DELETE ON harness_items
        WHEN json_extract(OLD.params, '$._projection') = 1
        BEGIN INSERT INTO restore_title_audit SELECT handle_state_json FROM worker_sessions WHERE id=OLD.worker_session_id; END")
        .execute(f.repo.db().writer_pool()).await.unwrap();
    let restored = restore_steered_entries_codex_dropped(&f.harness.inner, &turn)
        .await
        .unwrap();
    assert_eq!(restored, vec![id.clone()]);
    let captured: String = sqlx::query_scalar("SELECT snapshot FROM restore_title_audit")
        .fetch_one(f.repo.db().read_pool())
        .await
        .unwrap();
    let snapshot = HarnessSnapshot::parse_known(serde_json::from_str(&captured).unwrap()).unwrap();
    assert!(
        snapshot
            .pending_entries()
            .iter()
            .any(|entry| entry.id() == Some(&id)),
        "deleting the projection must not remove the only durable first you"
    );
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let card = f.harness.inner.card_id.to_string();
    assert_eq!(
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap()[&card]
            .as_deref(),
        Some("First visible steer")
    );
}

#[tokio::test]
async fn conversation_title_restored_first_you_keeps_projection_if_pending_persist_fails() {
    for (failed, unwritten) in [(false, false), (false, true), (true, false), (true, true)] {
        let (f, id, turn) = first_visible_steer().await;
        if failed {
            failed_owner(&f).await;
        }
        if unwritten {
            f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
        } else {
            sqlx::query(
                "CREATE TRIGGER reject_restore_snapshot BEFORE UPDATE ON worker_sessions
        BEGIN SELECT RAISE(ABORT, 'test pending persist failure'); END",
            )
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        }
        restore_steered_entries_codex_dropped(&f.harness.inner, &turn)
            .await
            .unwrap();
        assert!(f.stored().await.pending_entries().is_empty());
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_some(),
            "failed pending persistence must retain the only durable first you"
        );
        let card = f.harness.inner.card_id.to_string();
        assert_eq!(
            crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
                .await
                .unwrap()[&card]
                .as_deref(),
            Some("First visible steer")
        );
    }
}

#[tokio::test]
async fn conversation_title_pending_and_drained_input_use_same_renderer() {
    let f = Fixture::new().await;
    let card = f.harness.inner.card_id.to_string();
    f.enqueue(vec![QueueEntry::user_message(
        "Migrating input".into(),
        None,
        Vec::new(),
    )])
    .await;
    assert!(
        f.repo
            .conversation_title_context(std::slice::from_ref(&card))
            .await
            .unwrap()
            .sources[0]
            .input
            .is_none()
    );
    let captured =
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap();
    // Exercise the real issuance transfer, rather than another fixture copy of its writes.
    f.issue().await;
    assert!(f.stored().await.pending_entries().is_empty());
    assert_eq!(captured[&card].as_deref(), Some("Migrating input"));
    assert_eq!(
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap()[&card]
            .as_deref(),
        Some("Migrating input")
    );
}

#[tokio::test]
async fn conversation_title_drain_persisted_first_input_wins_over_captured_pending() {
    let f = Fixture::new().await;
    let card = f.harness.inner.card_id.to_string();
    f.enqueue(vec![QueueEntry::user_message(
        "Migrating input".into(),
        None,
        Vec::new(),
    )])
    .await;
    let durable = QueueEntry::user_message("Earlier durable input".into(), None, Vec::new());
    let segments =
        input_segments_for_entries(&f.harness.inner.card_id, std::slice::from_ref(&durable));
    insert_projection_row(
        &f.harness.inner,
        "thread-commit-wake",
        durable.id().unwrap().as_str(),
        &segments,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::conversation_metadata::titles(f.repo.as_ref(), std::slice::from_ref(&card))
            .await
            .unwrap()[&card]
            .as_deref(),
        Some("Earlier durable input")
    );
}

async fn failed_owner(f: &Fixture) {
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        planner_event(Notification::parse(
            "thread/status/changed".into(),
            json!({"threadId":"thread-commit-wake", "status":{"type":"systemError"}}),
        )),
    )
    .await
    .unwrap();
    assert!(
        matches!(&*f.harness.inner.state.lock().await, HarnessState::Wedged { reason, .. } if reason == HARNESS_SYSTEM_ERROR_REASON)
    );
}

#[tokio::test]
async fn conversation_title_restoration_delete_failure_rolls_back_pending_and_retries_both_owners()
{
    for failed in [false, true] {
        let (f, id, turn) = first_visible_steer().await;
        let entry = QueueEntry::user_message("Second rollback steer".into(), None, Vec::new());
        let second = entry.id().unwrap().clone();
        f.enqueue(vec![entry]).await;
        assert!(
            handle_steer(&f.harness.inner, &second, 0, &ActorId::Kernel)
                .await
                .unwrap()
                .is_ok()
        );
        if failed {
            failed_owner(&f).await;
        }
        sqlx::query(&format!(
            "CREATE TRIGGER reject_restored_delete BEFORE DELETE ON harness_items
            WHEN OLD.item_uuid='{second}'
            BEGIN SELECT RAISE(ABORT, 'test projection retirement failure'); END",
        ))
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
        restore_steered_entries_codex_dropped(&f.harness.inner, &turn)
            .await
            .unwrap();
        assert!(
            f.stored().await.pending_entries().is_empty(),
            "the entire transfer must roll back"
        );
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_some(),
            "rollback retains the unique durable source"
        );
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), second.as_str())
                .await
                .unwrap()
                .is_some(),
            "second source also survives rollback"
        );
        sqlx::query("DROP TRIGGER reject_restored_delete")
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        if failed {
            persist_failed_system_error_snapshot(&f.harness.inner)
                .await
                .unwrap();
        } else {
            persist_snapshot(&f.harness.inner).await.unwrap();
        }
        let stored = f.stored().await;
        assert_eq!(
            stored
                .pending_entries()
                .iter()
                .filter_map(|e| e.id())
                .collect::<Vec<_>>(),
            vec![&id, &second]
        );
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), second.as_str())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_none(),
            "a later owner persist must finish the transfer"
        );
    }
}

#[tokio::test]
async fn conversation_title_restoration_cancel_retains_unique_source_for_both_owners() {
    for failed in [false, true] {
        let (f, id, turn) = first_visible_steer().await;
        if failed {
            failed_owner(&f).await;
        }
        let mut blocker = crate::db::sqlite::begin_immediate_tx(f.repo.db())
            .await
            .unwrap();
        // The restoration can still read its projection; it stalls at acquiring its writer.
        sqlx::query("UPDATE worker_sessions SET updated_at_ms=updated_at_ms")
            .execute(&mut **blocker)
            .await
            .unwrap();
        let inner = f.harness.inner.clone();
        let task = tokio::spawn(async move {
            restore_steered_entries_codex_dropped(&inner, &turn)
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while f.harness.inner.restored_projections.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        blocker.rollback().await.unwrap();
        assert!(f.stored().await.pending_entries().is_empty());
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_some(),
            "cancel before persistence cannot delete the only source"
        );
        if failed {
            persist_failed_system_error_snapshot(&f.harness.inner)
                .await
                .unwrap();
        } else {
            persist_snapshot(&f.harness.inner).await.unwrap();
        }
        assert_eq!(f.stored().await.pending_entries()[0].id(), Some(&id));
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn conversation_title_failed_restoration_unwritten_guard_keeps_projection() {
    let (f, id, turn) = first_visible_steer().await;
    failed_owner(&f).await;
    // The existing failed-owner guard refuses harvested carriers.
    sqlx::query("UPDATE worker_sessions SET queue_harvested_at_ms=1 WHERE id=?")
        .bind(&f.harness.inner.worker_session_id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    restore_steered_entries_codex_dropped(&f.harness.inner, &turn)
        .await
        .unwrap();
    assert!(f.stored().await.pending_entries().is_empty());
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
            .await
            .unwrap()
            .is_some(),
        "unwritten failed snapshot must retain its only durable input"
    );
}

#[tokio::test]
async fn conversation_title_restoration_cancel_before_rebuffer_cannot_retire_a_source() {
    let (f, id, turn) = first_visible_steer().await;
    let debounce = f.harness.inner.debounce.lock().await;
    let inner = f.harness.inner.clone();
    let task = tokio::spawn(async move {
        restore_steered_entries_codex_dropped(&inner, &turn)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while f
            .harness
            .inner
            .historical_queue()
            .unwrap()
            .try_lock()
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(debounce);
    persist_snapshot(&f.harness.inner).await.unwrap();
    assert!(f.stored().await.pending_entries().is_empty());
    assert!(
        f.repo
            .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
            .await
            .unwrap()
            .is_some(),
        "retirement cannot be registered before the restored input is in the queue"
    );
}

mod requeue;

mod settlement;
