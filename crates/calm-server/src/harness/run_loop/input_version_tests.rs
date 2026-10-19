//! Version ordering is proved by the real queue, projection and provider reducer.
use super::completed_commit_tests::Fixture;
use super::input_inspection::InputInspection;
use super::input_inspection_recovery_tests::confirm_projection;
use super::*;
use calm_types::conversation_receipt::InitialInputUnavailableReason as Reason;

async fn start_running(fx: &Fixture) {
    let mut events = fx.harness.inner.backend.subscribe_events();
    fx.enqueue(vec![QueueEntry::user_message(
        "running turn".into(),
        None,
        vec![],
    )])
    .await;
    fx.issue().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let started = matches!(event.kind, PlannerEventKind::TurnStarted { .. });
        on_notification(
            &fx.harness.inner,
            &fx.harness.inner.live_claim.writer(),
            event,
        )
        .await
        .unwrap();
        if started {
            break;
        }
    }
}

async fn complete_running(fx: &Fixture) -> Result<()> {
    let snapshot = fx.harness.snapshot().await;
    let mut events = fx.harness.inner.backend.subscribe_events();
    fx.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::TurnCompleted {
            thread_id: snapshot.last_thread_id.unwrap(),
            turn: serde_json::json!({"id":snapshot.last_turn_id.unwrap(),"status":"completed"}),
        });
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event.kind, PlannerEventKind::TurnCompleted { .. }));
    on_notification(
        &fx.harness.inner,
        &fx.harness.inner.live_claim.writer(),
        event,
    )
    .await
}

#[tokio::test]
async fn versioned_input_inspection_requires_confirmation_of_the_highest_restored_revision() {
    let fx = Fixture::new().await;
    start_running(&fx).await;
    let entry = QueueEntry::user_message("actual restored input".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    let initial_rev = entry.user_view().unwrap().rev;
    fx.enqueue(vec![entry]).await;
    fx.harness
        .steer_pending_entry(id.clone(), initial_rev, ActorId::User)
        .await
        .unwrap()
        .unwrap();
    // A real failed cleanup leaves the first provisional row for a late provider echo.
    sqlx::query(concat!(
        "CREATE TRIGGER retain_old_input BEFORE DELETE ON ",
        "harness_items BEGIN SELECT RAISE(ABORT, 'retain old input'); END"
    ))
    .execute(fx.repo.pool())
    .await
    .unwrap();
    // Restoration and its final snapshot share the failed DELETE transaction.
    // The provisional source stays durable while the higher revision remains in memory.
    let error = complete_running(&fx).await.unwrap_err();
    assert!(error.to_string().contains("retain old input"), "{error}");
    assert!(fx.stored().await.pending_entries().is_empty());
    let restored = fx.harness.snapshot().await.pending_entries();
    let actual = restored
        .iter()
        .find_map(|entry| entry.user_view().filter(|entry| entry.id == &id))
        .unwrap();
    assert!(actual.rev > initial_rev);
    let restored_rev = actual.rev;
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    assert!(
        matches!(fx.harness.inspect_input(id.clone()).await.unwrap(), InputInspection::Queued { rev, .. } if rev == restored_rev)
    );
    fx.issue().await;
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable),
        "the older confirmed row cannot certify the higher unbound input"
    );
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    assert!(
        matches!(fx.harness.inspect_input(id).await.unwrap(), InputInspection::Projected { entry_rev, .. } if entry_rev == restored_rev)
    );
}

#[tokio::test]
async fn versioned_input_inspection_does_not_certify_below_a_live_rowless_steer() {
    let fx = Fixture::new().await;
    let original = QueueEntry::user_message("original accepted input".into(), None, vec![]);
    let id = original.id().unwrap().clone();
    let mut events = fx.harness.inner.backend.subscribe_events();
    fx.enqueue(vec![original.clone()]).await;
    fx.issue().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let started = matches!(event.kind, PlannerEventKind::TurnStarted { .. });
        on_notification(
            &fx.harness.inner,
            &fx.harness.inner.live_claim.writer(),
            event,
        )
        .await
        .unwrap();
        if started {
            break;
        }
    }
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    // Re-buffer the actual entry, then use the queue owner to create its next revision.
    fx.enqueue(vec![original]).await;
    let revised = fx
        .harness
        .mutate_pending_entry(
            QueueMutation::Edit {
                entry_id: id.clone(),
                text: "actual later input".into(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_versioned_steer BEFORE INSERT ON ",
        "harness_items BEGIN SELECT RAISE(ABORT, 'refuse higher input row'); END"
    ))
    .execute(fx.repo.pool())
    .await
    .unwrap();
    fx.harness
        .steer_pending_entry(id.clone(), revised.rev, ActorId::User)
        .await
        .unwrap()
        .unwrap();
    assert!(
        fx.harness
            .inner
            .steered_into_running_turn
            .lock()
            .await
            .iter()
            .any(|steered| steered.row_id.is_none()
                && steered
                    .entry
                    .user_view()
                    .is_some_and(|entry| entry.id == &id && entry.rev == revised.rev))
    );
    assert_eq!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}
