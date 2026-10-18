//! Durable generation, interrupted admission and original-owner reconciliation.
use super::completed_commit_tests::Fixture;
use super::handoff_coordinator_tests::{active, entered, progress, request, runtime, start_at};
use crate::harness::handoff_pause::{PauseState, ReleaseOutcome};
use crate::model::CardRole;
use crate::operation::handoff_coordinator::{self as coordinator, CoordinatorResult};

#[tokio::test]
async fn handoff_coordinator_pointer_aba_rejects_original_approval_before_any_pause() {
    for already_admitted in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let request = request(&a, &b).await;
        if already_admitted {
            coordinator::admit_for_test(a.repo.pool(), request.clone())
                .await
                .unwrap();
        }
        sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
            .bind(request.target_card_id.as_str())
            .execute(a.repo.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
            .bind(&request.target_session_id)
            .bind(request.target_card_id.as_str())
            .execute(a.repo.pool())
            .await
            .unwrap();
        if already_admitted {
            runtime.drive().await.unwrap();
            assert!(matches!(
                coordinator::query(&runtime, &request).await.unwrap(),
                CoordinatorResult::CancelledOrRefused
            ));
        } else {
            assert!(
                coordinator::admit_for_test(a.repo.pool(), request)
                    .await
                    .is_err()
            );
        }
        assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
        assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
        assert_eq!(active(&a).await, 0);
    }
}

#[tokio::test]
async fn handoff_coordinator_recovered_owner_release_remains_health_unconfirmed() {
    let mut a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (_id, hook, drive) = start_at(&runtime, &request, "SourcePaused").await;
    entered(&hook).await;
    super::handoff_pause_fault_tests::restart(&mut a).await;
    registry.insert(request.source_session_id.clone(), a.harness.clone());
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::NeedsRecovery { .. }
    ));
    assert!(matches!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            outcome: ReleaseOutcome::HealthUnconfirmedAfterRecovery,
            ..
        }
    ));
    assert_eq!(active(&a).await, 1);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    a.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_coordinator_interrupted_intent_before_send_uses_serial_absence_barrier() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (id, hook, drive) = start_at(&runtime, &request, "SourceIntent").await;
    entered(&hook).await;
    drive.abort();
    assert!(drive.await.unwrap_err().is_cancelled());
    hook.release.notify_one();
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
    coordinator::cancel(&runtime, &request).await.unwrap();
    let recovery = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(recovery).await.unwrap();
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
    assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
    assert_eq!(
        progress(&a, &id).await["claims"]["source"]["state"],
        "untouched"
    );
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_preparation_storage_failure_retries_original_generation_without_effects()
 {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TRIGGER fail_prepare BEFORE UPDATE OF tx_output_json ON operations WHEN
            OLD.kind='handoff-workflow' BEGIN SELECT RAISE(ABORT,'prepare storage unavailable');
            END
        "#,
    )
    .execute(a.repo.pool())
    .await
    .unwrap();
    runtime.drive().await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::NeedsRecovery { .. }
    ));
    assert_eq!(active(&a).await, 1);
    sqlx::query("DROP TRIGGER fail_prepare")
        .execute(a.repo.pool())
        .await
        .unwrap();
    coordinator::cancel(&runtime, &request).await.unwrap();
    let op = runtime
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
        .await
        .unwrap()
        .unwrap();
    coordinator::retry(&runtime, &request, op.attempt)
        .await
        .unwrap();
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
}

#[tokio::test]
async fn handoff_coordinator_busy_driver_never_persists_approval_waiting_for_idle() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let driver = runtime.lock_for_track_delete().await;
    assert!(coordinator::begin(&runtime, request).await.is_err());
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
    drop(driver);
}

#[tokio::test]
async fn handoff_coordinator_queued_user_input_is_rejected_at_sql_admission() {
    for source in [true, false] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let request = request(&a, &b).await;
        let busy = if source { &a } else { &b };
        busy.enqueue(vec![crate::harness::QueueEntry::user_message(
            "already busy".into(),
            None,
            vec![],
        )])
        .await;
        assert!(coordinator::begin(&runtime, request).await.is_err());
        assert_eq!(active(&a).await, 0);
        assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
        assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
    }
}

#[tokio::test]
async fn handoff_coordinator_activation_unknown_only_queries_original_native_intent() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    a.daemon.fail_next_thread_start_for_test();
    coordinator::begin(&runtime, request.clone()).await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: crate::operation::handoff_activation::ActivationResult::Unknown { .. },
            ..
        }
    ));
    let native = a.daemon.started_thread_params_for_test();
    coordinator::begin(&runtime, request.clone()).await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: crate::operation::handoff_activation::ActivationResult::Unknown { .. },
            ..
        }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test(), native);
    assert_eq!(active(&a).await, 1);
}

#[tokio::test]
async fn handoff_coordinator_inflight_service_replay_returns_original_without_wait_or_new_claim() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (id, hook, drive) = start_at(&runtime, &request, "SourcePaused").await;
    entered(&hook).await;
    assert_eq!(
        coordinator::begin(&runtime, request.clone()).await.unwrap(),
        id
    );
    let mut changed = request.clone();
    changed.user_audit_id = "different-scope".into();
    assert!(coordinator::begin(&runtime, changed).await.is_err());
    let mut competing = request.clone();
    competing.key = crate::model::new_id();
    assert!(coordinator::begin(&runtime, competing).await.is_err());
    assert_eq!(active(&a).await, 1);
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert_eq!(active(&a).await, 0);
}
