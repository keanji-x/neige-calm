//! Durable generation, interrupted admission and original-owner reconciliation.
use super::completed_commit_tests::Fixture;
use super::handoff_coordinator_tests::{active, entered, progress, request, runtime, start_at};
use crate::db::prelude::*;
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
            coordinator::admit_for_test(a.repo.db(), request.clone())
                .await
                .unwrap();
        }
        sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
            .bind(request.target_card_id.as_str())
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
        sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
            .bind(&request.target_session_id)
            .bind(request.target_card_id.as_str())
            .execute(a.repo.db().writer_pool())
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
                coordinator::admit_for_test(a.repo.db(), request)
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
async fn handoff_coordinator_recovered_owner_release_settles_without_claiming_health() {
    let mut a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (id, hook, drive) = start_at(&runtime, &request, "SourcePaused").await;
    entered(&hook).await;
    super::handoff_pause_fault_tests::restart(&mut a).await;
    registry.insert(request.source_session_id.clone(), a.harness.clone());
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert!(matches!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            outcome: ReleaseOutcome::HealthUnconfirmedAfterRecovery,
            ..
        }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    assert_unhealthy_settlement(&mut a, &b, &runtime, &registry, &request, &id).await;
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
    coordinator::admit_for_test(a.repo.db(), request.clone())
        .await
        .unwrap();
    sqlx::query(
        r#"
            CREATE TRIGGER fail_prepare BEFORE UPDATE OF tx_output_json ON operations WHEN
            OLD.kind='handoff-workflow' BEGIN SELECT RAISE(ABORT,'prepare storage unavailable');
            END
        "#,
    )
    .execute(a.repo.db().writer_pool())
    .await
    .unwrap();
    runtime.drive().await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::NeedsRecovery { reason } if !reason.is_empty()
    ));
    assert_eq!(active(&a).await, 1);
    sqlx::query("DROP TRIGGER fail_prepare")
        .execute(a.repo.db().writer_pool())
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

// Exercise retry, terminal absorption, reservation rollback and ordinary provider recovery.
pub(super) async fn assert_unhealthy_settlement(
    a: &mut Fixture,
    b: &Fixture,
    runtime: &std::sync::Arc<crate::operation::OperationRuntime>,
    registry: &crate::harness::HarnessRegistry,
    request: &coordinator::UserHandoffRequest,
    id: &str,
) {
    let released = a.stored().await.handoff_pause;
    let op = runtime
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
        .await
        .unwrap()
        .unwrap();
    let result = coordinator::retry(runtime, request, op.attempt)
        .await
        .unwrap();
    let terminal = progress(a, id).await;
    assert_eq!(terminal["state"], "finished");
    assert!(matches!(result, CoordinatorResult::CancelledOrRefused));
    assert_eq!(active(a).await, 0);
    assert_eq!(
        terminal["claims"]["source"],
        serde_json::to_value(&released).unwrap()
    );
    for _ in 0..2 {
        assert!(matches!(
            coordinator::retry(runtime, request, op.attempt)
                .await
                .unwrap(),
            CoordinatorResult::CancelledOrRefused
        ));
        assert_eq!(progress(a, id).await, terminal);
        assert_eq!(a.stored().await.handoff_pause, released);
        assert_eq!(active(a).await, 0);
    }
    // The SQL terminal guard must preserve the original nonhealthy evidence.
    assert!(
        sqlx::query(concat!(
            "UPDATE operations SET tx_output_json=json_set(tx_output_json,",
            "'$.result.claims.source.outcome.kind','healthy_released') WHERE id=?1",
        ))
        .bind(id)
        .execute(a.repo.db().writer_pool())
        .await
        .is_err()
    );
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
        .bind(request.track_id.as_str())
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?1")
        .bind(request.track_id.as_str())
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    let mut next = super::handoff_coordinator_tests::request(a, b).await;
    next.key = crate::model::new_id();
    assert!(
        coordinator::begin(runtime, next.clone()).await.is_err(),
        "released does not grant provider readiness"
    );
    // Use the same recovery owner as human Send, then reload through real harness recovery.
    let row = a
        .repo
        .session_projection_by_id(&request.source_session_id)
        .await
        .unwrap()
        .unwrap();
    let wiring = crate::harness::wiring::PlannerWiring::unconfigured_for_test(a.repo.clone());
    crate::harness::backend::factory::PlannerFactory::new(
        std::sync::Arc::downgrade(&a.daemon),
        &wiring.claude,
        &wiring.acp,
    )
    .resume_system_error_conversation(a.repo.as_ref(), &row, async || {
        a.harness.quiesce_system_error_for_recovery().await
    })
    .await
    .unwrap();
    super::handoff_pause_fault_tests::restart(a).await;
    registry.insert(request.source_session_id.clone(), a.harness.clone());
    assert_eq!(a.stored().await.handoff_pause, released);
    // Cancel a newly admitted approval before driving, to avoid creating a provider turn.
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("handoff-begin-admitted", &next.key, hook.clone());
    let begin = {
        let runtime = runtime.clone();
        let next = next.clone();
        tokio::spawn(async move { coordinator::begin(&runtime, next).await })
    };
    entered(&hook).await;
    let next_id = runtime
        .find_by_kind_and_idempotency("handoff-workflow", &next.key)
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_ne!(next_id, id);
    assert_eq!(active(a).await, 1);
    coordinator::cancel(runtime, &next).await.unwrap();
    hook.release.notify_one();
    assert_eq!(begin.await.unwrap().unwrap(), next_id);
    assert_eq!(active(a).await, 0);
    assert_eq!(progress(a, id).await, terminal);
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
        .bind(request.track_id.as_str())
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    a.repo
        .track_delete(request.track_id.as_str())
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_coordinator_target_unhealthy_release_settlement_is_atomic() {
    for recovered in [false, true] {
        let a = Fixture::new().await;
        let mut b = a.sibling(CardRole::Assistant).await;
        b.harness = crate::harness::PlannerHarness::run(super::handoff_pause_tests::params(
            &b,
            b.stored().await,
        ));
        let (runtime, registry) = runtime(&a, &b);
        let request = request(&a, &b).await;
        let (id, hook, drive) = start_at(&runtime, &request, "BothPaused").await;
        entered(&hook).await;
        if recovered {
            super::handoff_pause_fault_tests::restart(&mut b).await;
            registry.insert(request.target_session_id.clone(), b.harness.clone());
        } else {
            super::handoff_pause_fault_tests::error(&b).await;
        }
        sqlx::query(concat!(
            "CREATE TRIGGER refuse_settlement BEFORE UPDATE OF active ON planner_handoff_coordinators ",
            "WHEN OLD.active=1 AND NEW.active=0 BEGIN SELECT RAISE(IGNORE); END",
        ))
            .execute(a.repo.db().writer_pool()).await.unwrap();
        coordinator::cancel(&runtime, &request).await.unwrap();
        hook.release.notify_one();
        drive.await.unwrap();
        let released = b.stored().await.handoff_pause;
        assert!(matches!(&released, PauseState::Released { outcome, .. }
            if !matches!(outcome, ReleaseOutcome::HealthyReleased)));
        assert_eq!(active(&a).await, 1);
        assert_eq!(
            progress(&a, &id).await["state"],
            "compensating",
            "terminal and reservation release must roll back together"
        );
        sqlx::query("DROP TRIGGER refuse_settlement")
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
        let op = runtime
            .find_by_kind_and_idempotency("handoff-workflow", &request.key)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            coordinator::retry(&runtime, &request, op.attempt)
                .await
                .unwrap(),
            CoordinatorResult::CancelledOrRefused
        ));
        let terminal = progress(&a, &id).await;
        assert_eq!(terminal["state"], "finished");
        assert_eq!(
            terminal["claims"]["target"],
            serde_json::to_value(&released).unwrap()
        );
        assert_eq!(active(&a).await, 0);
        assert_eq!(b.stored().await.handoff_pause, released);
        let original = released.receipt().unwrap().request().clone();
        assert!(
            b.harness.pause_for_handoff(original).await.is_err(),
            "a settled original writer cannot acquire authority again"
        );
        assert_eq!(progress(&a, &id).await, terminal);
        b.harness.shutdown().await.unwrap();
    }
}
