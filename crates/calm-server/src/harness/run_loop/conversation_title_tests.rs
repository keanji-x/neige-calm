//! Restored steers can be the first visible you after a system-only turn.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::codex_appserver::Notification;
use crate::db::RepoRead;
use crate::db::prelude::*;
use crate::harness::codex_events::planner_event;
use serde_json::json;

async fn first_visible_steer() -> (Fixture, QueueEntryId, String) {
    let f = Fixture::new().await;
    let (_, system) = f
        .event_entry(&Event::TaskCompleted {
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
        .execute(f.repo.pool())
        .await
        .unwrap();
    // Capture the actual durable snapshot at the deletion boundary, not the final state.
    sqlx::query("CREATE TRIGGER restore_title_deleted AFTER DELETE ON harness_items
        WHEN json_extract(OLD.params, '$._projection') = 1
        BEGIN INSERT INTO restore_title_audit SELECT handle_state_json FROM worker_sessions WHERE id=OLD.worker_session_id; END")
        .execute(f.repo.pool()).await.unwrap();
    let restored = restore_steered_entries_codex_dropped(&f.harness.inner, &turn).await;
    assert_eq!(restored, vec![id.clone()]);
    let captured: String = sqlx::query_scalar("SELECT snapshot FROM restore_title_audit")
        .fetch_one(f.repo.pool())
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
    for unwritten in [false, true] {
        let (f, id, turn) = first_visible_steer().await;
        if unwritten {
            f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
        } else {
            sqlx::query(
                "CREATE TRIGGER reject_restore_snapshot BEFORE UPDATE ON worker_sessions
        BEGIN SELECT RAISE(ABORT, 'test pending persist failure'); END",
            )
            .execute(f.repo.pool())
            .await
            .unwrap();
        }
        restore_steered_entries_codex_dropped(&f.harness.inner, &turn).await;
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
