//! Release callers exercise the owner's settlement contract, not a copied lifecycle.
use super::*;
use crate::harness::HarnessRegistry;

#[tokio::test]
async fn conversation_title_result_missing_row_keeps_its_owner() {
    let (f, _, turn) = first_visible_steer().await;
    let entry = QueueEntry::user_message("Unknown transfer is still owed".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    f.enqueue(vec![entry]).await;
    sqlx::query(
        "CREATE TRIGGER refuse_result_checkpoint BEFORE UPDATE OF handle_state_json \
         ON worker_sessions BEGIN SELECT RAISE(ABORT, 'checkpoint unavailable'); END",
    )
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(
        handle_steer(&f.harness.inner, &id, 0, &ActorId::User)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(matches!(
        &*f.harness.inner.input_commit.lock().await,
        Some(input_commit::Continuation::SteerAccepted(outcome))
            if outcome.entry_id == id && outcome.turn_id == turn
    ));
    sqlx::query("DROP TRIGGER refuse_result_checkpoint")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    // Storage disappearance is not a committed start/MOVE. It cannot excuse the
    // actual accepted result or remove its discoverable input owner.
    let mut tx = crate::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    crate::db::sqlite::session_delete_tx(&mut tx, &f.harness.inner.worker_session_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let registry = HarnessRegistry::new();
    let runtime = f.harness.inner.worker_session_id.clone();
    registry.insert(runtime.clone(), f.harness.clone());
    assert!(registry.shutdown_and_remove(&runtime).await.is_err());
    assert!(registry.get(&runtime).is_some());
    assert!(matches!(
        &*f.harness.inner.input_commit.lock().await,
        Some(input_commit::Continuation::SteerAccepted(outcome)) if outcome.entry_id == id
    ));
    assert_eq!(f.daemon.turn_start_count_for_test(), 1);
}

async fn owed_failed_pair() -> (Fixture, HarnessRegistry, Vec<QueueEntryId>) {
    let (f, first, turn) = first_visible_steer().await;
    let second = QueueEntry::user_message("Second release steer".into(), None, vec![]);
    let id = second.id().unwrap().clone();
    f.enqueue(vec![second]).await;
    assert!(
        handle_steer(&f.harness.inner, &id, 0, &ActorId::User)
            .await
            .unwrap()
            .is_ok()
    );
    failed_owner(&f).await;
    f.reject_projection_retirement().await;
    assert!(
        on_notification(
            &f.harness.inner,
            &f.harness.inner.live_claim.writer(),
            planner_event(Notification::parse(
                "turn/completed".into(),
                json!({
                    "threadId":"thread-commit-wake", "turn":{"id":turn,"status":"failed"}
                })
            )),
        )
        .await
        .is_err()
    );
    assert!(f.stored().await.pending_entries().is_empty());
    assert_eq!(
        *f.harness.inner.restored_projections.lock().await,
        vec![first.clone(), id.clone()]
    );
    let registry = HarnessRegistry::new();
    registry.insert(f.harness.inner.worker_session_id.clone(), f.harness.clone());
    (f, registry, vec![first, id])
}

async fn allow_retirement(f: &Fixture) {
    sqlx::query("DROP TRIGGER refuse_projection_cleanup")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
}

async fn assert_sources_owed(f: &Fixture, registry: &HarnessRegistry, ids: &[QueueEntryId]) {
    assert!(registry.get(&f.harness.inner.worker_session_id).is_some());
    assert!(f.stored().await.pending_entries().is_empty());
    assert_eq!(*f.harness.inner.restored_projections.lock().await, ids);
    for id in ids {
        assert!(
            f.repo
                .transcript_projection_id(f.harness.inner.card_id.as_str(), id.as_str())
                .await
                .unwrap()
                .is_some()
        );
    }
}

async fn assert_settled(f: &Fixture, ids: &[QueueEntryId]) {
    assert!(f.harness.inner.restored_projections.lock().await.is_empty());
    assert_eq!(
        f.stored()
            .await
            .pending_entries()
            .iter()
            .filter_map(|e| e.id())
            .collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
    for id in ids {
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
async fn conversation_title_owner_release_retries_every_registry_exit() {
    // Normal remove, replacement reservation and strict track deletion all retain
    // the same live slot until the shared owner's real transaction succeeds.
    for exit in 0..3 {
        let (f, registry, ids) = owed_failed_pair().await;
        let runtime = f.harness.inner.worker_session_id.clone();
        let failed = match exit {
            0 => registry.shutdown_and_remove(&runtime).await.is_err(),
            1 => registry
                .reserve_after_shutdown(runtime.clone())
                .await
                .is_err(),
            _ => registry
                .shutdown_track(&f.harness.inner.track_id, f.daemon.thread_seals().clone())
                .await
                .is_err(),
        };
        assert!(failed);
        assert_sources_owed(&f, &registry, &ids).await;
        allow_retirement(&f).await;
        match exit {
            0 => {
                assert!(
                    registry
                        .shutdown_and_remove(&runtime)
                        .await
                        .unwrap()
                        .is_some()
                );
            }
            1 => {
                let reservation = registry
                    .reserve_after_shutdown(runtime.clone())
                    .await
                    .unwrap();
                drop(reservation);
            }
            _ => {
                let seals = registry
                    .shutdown_track(&f.harness.inner.track_id, f.daemon.thread_seals().clone())
                    .await
                    .unwrap();
                for seal in seals {
                    f.daemon.thread_seals().unseal_after_rollback(&seal);
                }
            }
        }
        assert!(registry.get(&runtime).is_none());
        assert_settled(&f, &ids).await;
    }
}

#[tokio::test]
async fn conversation_title_owner_release_closing_quiesce_refuses_unwritten_guard() {
    let (f, registry, ids) = owed_failed_pair().await;
    assert!(f.harness.shutdown().await.is_err());
    allow_retirement(&f).await;
    sqlx::query("UPDATE worker_sessions SET queue_harvested_at_ms=1 WHERE id=?")
        .bind(&f.harness.inner.worker_session_id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(f.harness.quiesce_system_error_for_recovery().await.is_err());
    assert_sources_owed(&f, &registry, &ids).await;
    // Restore the original row's eligibility; do not reset closing or checkpoint.
    sqlx::query("UPDATE worker_sessions SET queue_harvested_at_ms=NULL WHERE id=?")
        .bind(&f.harness.inner.worker_session_id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.harness.quiesce_system_error_for_recovery().await.unwrap();
    assert_settled(&f, &ids).await;
}

#[tokio::test]
async fn conversation_title_owner_release_cancelled_shutdown_retains_quiesce_retry() {
    let (f, registry, ids) = owed_failed_pair().await;
    let mut blocker = crate::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET updated_at_ms=updated_at_ms")
        .execute(&mut **blocker)
        .await
        .unwrap();
    let handle = f.harness.clone();
    let mut shutdown = tokio::spawn(async move { handle.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut shutdown)
            .await
            .is_err()
    );
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    blocker.rollback().await.unwrap();
    assert_sources_owed(&f, &registry, &ids).await;
    assert!(f.harness.quiesce_system_error_for_recovery().await.is_err());
    assert_sources_owed(&f, &registry, &ids).await;
    allow_retirement(&f).await;
    f.harness.quiesce_system_error_for_recovery().await.unwrap();
    assert_settled(&f, &ids).await;
}

#[tokio::test]
async fn conversation_title_owner_release_cancelled_wait_finishes_observation_before_quiesce() {
    let f = Fixture::new().await;
    failed_owner(&f).await;
    let inner = &f.harness.inner;
    let owner = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: inner.worker_session_id.clone(),
        card_id: inner.card_id.clone(),
        track_id: inner.track_id.clone(),
        thread_id: inner.thread_id.read().await.clone(),
        repo: f.repo.clone(),
        events: inner.events.clone(),
        card_role_cache: inner.card_role_cache.clone(),
        track_area_cache: inner.track_area_cache.clone(),
        backend: PlannerBackend::codex(
            f.daemon.clone(),
            inner.card_id.as_str(),
            inner.track_id.as_str(),
            &inner.worker_session_id,
        )
        .unwrap(),
        registry: HarnessRegistry::new(),
        config: HarnessConfig::default(),
        snapshot: f.stored().await.try_into().unwrap(),
    })
    .await
    .unwrap();
    let (event_id, _) = f
        .event_entry(&Event::TaskCompleted {
            notification: calm_types::event::PlannerNotification::Notify,
            idempotency_key: "release-observation".into(),
            result: json!({"summary":"retained completion"}),
            artifacts: vec![],
            agent_message: None,
        })
        .await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    install_planner_harness_observation_race_hook_for_test(
        &inner.worker_session_id,
        PlannerHarnessObservationRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    owner
        .observe_envelope(
            Observation::TaskCompleted {
                idempotency_key: "release-observation".into(),
                result: json!({"summary":"retained completion"}),
                result_contract: None,
            },
            event_id,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert_eq!(owner.snapshot().await.unwrap().push_watermark, event_id);
    assert!(owner.snapshot().await.unwrap().pending_entries().is_empty());
    let registry = HarnessRegistry::new();
    registry.insert(inner.worker_session_id.clone(), owner.clone());
    let registry_task = registry.clone();
    let runtime = inner.worker_session_id.clone();
    let mut shutdown =
        tokio::spawn(async move { registry_task.shutdown_and_remove(&runtime).await });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut shutdown).await;
    // Always release the real notification, even if a regression returned early.
    shutdown.abort();
    let _ = shutdown.await;
    release.notify_one();
    assert!(
        early.is_err(),
        "shutdown must wait for the complete observation"
    );
    assert!(registry.get(&inner.worker_session_id).is_some());
    owner.quiesce_system_error_for_recovery().await.unwrap();
    let stored = f.stored().await;
    assert_eq!(stored.push_watermark, event_id);
    assert_eq!(
        stored
            .pending_entries()
            .iter()
            .filter(|entry| entry.envelope_id() == Some(event_id))
            .count(),
        1
    );
}

#[tokio::test]
async fn conversation_title_owner_release_durable_pending_needs_no_checkpoint() {
    let f = Fixture::new().await;
    f.enqueue(vec![QueueEntry::user_message(
        "Already owned by the durable queue".into(),
        None,
        vec![],
    )])
    .await;
    let before = serde_json::to_value(f.stored().await).unwrap();
    // Even a nonempty live queue is not an outstanding transfer. Shutdown must
    // neither require nor manufacture another durable snapshot.
    sqlx::query(
        "CREATE TRIGGER refuse_release_checkpoint BEFORE UPDATE ON worker_sessions
        BEGIN SELECT RAISE(ABORT, 'physical stop must not checkpoint'); END",
    )
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    f.harness.shutdown().await.unwrap();
    assert_eq!(serde_json::to_value(f.stored().await).unwrap(), before);
}

#[tokio::test]
async fn conversation_title_owner_release_active_requeue_keeps_source_until_commit() {
    let (f, id, turn) = first_visible_steer().await;
    f.reject_projection_retirement().await;
    assert_eq!(
        restore_steered_entries_codex_dropped(&f.harness.inner, &turn)
            .await
            .unwrap(),
        vec![id.clone()]
    );
    let registry = HarnessRegistry::new();
    let runtime = f.harness.inner.worker_session_id.clone();
    registry.insert(runtime.clone(), f.harness.clone());
    assert!(registry.shutdown_and_remove(&runtime).await.is_err());
    assert_sources_owed(&f, &registry, std::slice::from_ref(&id)).await;
    allow_retirement(&f).await;
    registry.shutdown_and_remove(&runtime).await.unwrap();
    assert!(registry.get(&runtime).is_none());
    assert_settled(&f, std::slice::from_ref(&id)).await;
}
