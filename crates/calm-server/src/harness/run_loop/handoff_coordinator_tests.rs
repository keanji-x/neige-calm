//! Real audited admission -> OperationRuntime -> serial pause owners -> SQL -> activation.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::runtime_with_repo;
use super::handoff_pause_tests::system;
use super::*;
use crate::db::prelude::*;
use crate::harness::handoff_pause::{PauseState, ReleaseOutcome};
use crate::model::CardRole;
use crate::operation::handoff_activation::ActivationResult;
use crate::operation::handoff_coordinator::{self as coordinator, CoordinatorResult};
use crate::operation::{OperationReceipt, OperationRuntime};

pub(super) use super::handoff_activation_test_support::coordinator_request as request;
pub(super) fn runtime(
    a: &Fixture,
    b: &Fixture,
) -> (Arc<OperationRuntime>, crate::harness::HarnessRegistry) {
    let registry = crate::harness::HarnessRegistry::new();
    registry.insert(a.harness.inner.worker_session_id.clone(), a.harness.clone());
    registry.insert(b.harness.inner.worker_session_id.clone(), b.harness.clone());
    (
        Arc::new(runtime_with_repo(a, a.repo.clone(), registry.clone())),
        registry,
    )
}
pub(super) async fn active(a: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM planner_handoff_coordinators WHERE active=1")
        .fetch_one(a.repo.pool())
        .await
        .unwrap()
}
pub(super) async fn progress(a: &Fixture, id: &str) -> serde_json::Value {
    let json: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(id)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    serde_json::from_str::<serde_json::Value>(&json).unwrap()["result"].clone()
}
pub(super) fn hook(id: &str, stage: &str) -> crate::test_seams::PausePoint {
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "handoff-coordinator-checkpoint",
        &format!("{id}:{stage}"),
        hook.clone(),
    );
    hook
}
pub(super) async fn entered(hook: &crate::test_seams::PausePoint) {
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_coordinator_is_declared_in_the_production_operation_contract() {
    let a = Fixture::new().await;
    let runtime = super::handoff_cutover_tests::runtime_for_repo(a.repo.clone());
    assert!(
        runtime
            .registered_adapter_kinds()
            .any(|kind| kind == "handoff-coordinator")
    );
}

#[tokio::test]
async fn handoff_coordinator_real_pipeline_preserves_workers_history_and_system_queue() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let worker = a.sibling(CardRole::Worker).await;
    let before = a
        .repo
        .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    crate::operation::launch_cleanup_test_support::claimed_task(a.repo.clone(), a.harness.inner.events.clone(),
        crate::state::WriteContext::new(a.harness.inner.card_role_cache.clone(),a.harness.inner.track_area_cache.clone()),
        a.harness.inner.track_id.as_str(),serde_json::json!({"key":"handoff-unrelated","kind":"terminal","command":"true","ready":true,"declared_by":"user"})).await;
    let task_facts = super::handoff_cutover_tests::data_image(a.repo.pool())
        .await
        .into_iter()
        .filter(|(name, _)| {
            name.contains("task") || name.contains("ask") || name.contains("approval")
        })
        .collect::<Vec<_>>();
    system(&a, 9).await;
    let (runtime, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::begin(&runtime, request.clone()).await.unwrap();
    let CoordinatorResult::Forward {
        cutover,
        activation,
    } = coordinator::query(&runtime, &request).await.unwrap()
    else {
        panic!("must forward")
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        planner_session_id,
        ..
    } = *cutover;
    assert_eq!(
        activation,
        ActivationResult::Activated {
            session_id: planner_session_id.clone()
        }
    );
    let stored = a
        .repo
        .session_get_by_id(&planner_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    let snapshot = HarnessSnapshot::parse_known(stored.handle_state_json.unwrap()).unwrap();
    assert_eq!(snapshot.push_watermark, 9);
    let pending = serde_json::to_string(&snapshot).unwrap();
    let issued = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
    assert!(pending.contains("system 9") || issued.contains("system 9"));
    assert!(registry.get(&planner_session_id).is_some());
    for f in [&a, &b] {
        assert!(matches!(
            super::handoff_activation_test_support::stored(f, &f.harness.inner.worker_session_id)
                .await
                .handoff_pause,
            PauseState::ConsumedByHandoff { .. }
        ));
        assert!(
            a.repo
                .session_get_by_id(&f.harness.inner.worker_session_id.as_str().into())
                .await
                .unwrap()
                .unwrap()
                .mcp_token_hash
                .is_none()
        );
    }
    assert_eq!(
        a.repo
            .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap(),
        before
    );
    let after = super::handoff_cutover_tests::data_image(a.repo.pool())
        .await
        .into_iter()
        .filter(|(name, _)| {
            name.contains("task") || name.contains("ask") || name.contains("approval")
        })
        .collect::<Vec<_>>();
    assert_eq!(after, task_facts);
    assert_eq!(active(&a).await, 0);
    assert_eq!(
        coordinator::begin(&runtime, request.clone()).await.unwrap(),
        id
    );
    coordinator::cancel(&runtime, &request).await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
}

#[tokio::test]
async fn handoff_coordinator_atomic_same_key_scope_and_competing_key_reservation() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let request = request(&a, &b).await;
    let pool = a.repo.pool();
    let (left, right) = tokio::join!(
        coordinator::admit_for_test(pool, request.clone()),
        coordinator::admit_for_test(pool, request.clone())
    );
    let id = left.unwrap();
    assert_eq!(right.unwrap(), id);
    let mut changed = request.clone();
    changed.user_audit_id = "another approval".into();
    assert!(coordinator::admit_for_test(pool, changed).await.is_err());
    let mut other = request.clone();
    other.key = crate::model::new_id();
    assert!(coordinator::admit_for_test(pool, other).await.is_err());
    assert_eq!(active(&a).await, 1);
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
    assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
    assert!(
        sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
            .bind(request.track_id.as_str())
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM tracks WHERE id=?1")
            .bind(request.track_id.as_str())
            .execute(pool)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_coordinator_rejects_wrong_authority_exact_scope_and_busy_before_effects() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let original = request(&a, &b).await;
    for changed in 0..5 {
        let mut r = original.clone();
        r.key = crate::model::new_id();
        match changed {
            0 => r.actor = ActorId::Kernel,
            1 => r.source_session_id = crate::model::new_id(),
            2 => r.track_id = crate::model::new_id().into(),
            3 => r.user_audit_id.clear(),
            _ => r.target_card_id = r.source_card_id.clone(),
        };
        assert!(coordinator::admit_for_test(a.repo.pool(), r).await.is_err());
    }
    sqlx::query("UPDATE worker_sessions SET state='running' WHERE id=?1")
        .bind(&original.target_session_id)
        .execute(a.repo.pool())
        .await
        .unwrap();
    assert!(
        coordinator::admit_for_test(a.repo.pool(), original)
            .await
            .is_err()
    );
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
}

#[tokio::test]
async fn handoff_coordinator_second_pause_busy_releases_only_exact_first_claim() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = hook(&id, "SourcePaused");
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    b.enqueue(vec![QueueEntry::user_message("busy".into(), None, vec![])])
        .await;
    hook.release.notify_one();
    drive.await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::CancelledOrRefused
    ));
    assert!(matches!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            outcome: ReleaseOutcome::HealthyReleased,
            ..
        }
    ));
    assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
    let p = progress(&a, &id).await;
    assert_eq!(p["source_attempted"], true);
    assert_eq!(p["target_attempted"], false);
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
}

#[tokio::test]
async fn handoff_coordinator_cancel_at_each_precommit_checkpoint_compensates_original_claims() {
    for stage in [
        "SourceIntent",
        "SourcePaused",
        "TargetIntent",
        "BothPaused",
        "PlanRecorded",
        "CommitIntent",
    ] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let request = request(&a, &b).await;
        let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
            .await
            .unwrap();
        let hook = hook(&id, stage);
        let drive = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.drive().await.unwrap() })
        };
        entered(&hook).await;
        coordinator::cancel(&runtime, &request).await.unwrap();
        hook.release.notify_one();
        drive.await.unwrap();
        assert!(
            matches!(
                coordinator::query(&runtime, &request).await.unwrap(),
                CoordinatorResult::CancelledOrRefused
            ),
            "{stage}"
        );
        assert!(!a.stored().await.handoff_pause.is_paused(), "{stage}");
        assert!(!b.stored().await.handoff_pause.is_paused(), "{stage}");
        assert_eq!(active(&a).await, 0, "{stage}");
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    }
}

#[tokio::test]
async fn handoff_coordinator_intents_are_durable_before_any_real_pause() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
        a.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    let p = progress(&a, &id).await;
    assert_eq!(p["stage"], "source_intent");
    assert_eq!(p["source_attempted"], true);
    let payload: String = sqlx::query_scalar("SELECT payload_json FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(payload["source"]["claim_id"].as_str().is_some());
    assert!(payload["target"]["claim_id"].as_str().is_some());
    assert!(payload["plan_id"].as_str().is_some());
    assert!(payload["new_session_id"].as_str().is_some());
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_cutover_commit_reply_failure_queries_success_without_release() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    // Fail only the coordinator's postcommit checkpoint. Atomic cutover remains committed.
    sqlx::query(
        r#"
            CREATE TRIGGER fail_coordinator_committed BEFORE UPDATE OF tx_output_json ON operations
            WHEN OLD.kind='handoff-coordinator' AND
            json_extract(NEW.tx_output_json,'$.result.stage')='committed' BEGIN SELECT
            RAISE(ABORT,'lost postcommit checkpoint'); END
        "#,
    )
    .execute(a.repo.pool())
    .await
    .unwrap();
    runtime.drive().await.unwrap();
    for f in [&a, &b] {
        assert!(matches!(
            super::handoff_activation_test_support::stored(f, &f.harness.inner.worker_session_id)
                .await
                .handoff_pause,
            PauseState::ConsumedByHandoff { .. }
        ));
    }
    assert_eq!(
        progress(&a, &id).await["stage"],
        "commit_intent",
        "a possibly committed cutover never enters compensation"
    );
    assert_eq!(active(&a).await, 1);
    sqlx::query("DROP TRIGGER fail_coordinator_committed")
        .execute(a.repo.pool())
        .await
        .unwrap();
    let OperationReceipt::AwaitingRetry { attempt, .. } =
        runtime.observe_operation(&id).await.unwrap()
    else {
        panic!("recovery journal required")
    };
    coordinator::retry(&runtime, &request, attempt)
        .await
        .unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_dropped_pause_answer_recovers_original_claim_without_repause() {
    let mut a = Fixture::new().await;
    a.harness = PlannerHarness::run(super::handoff_pause_tests::params(&a, a.stored().await));
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
        a.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    drive.abort();
    assert!(drive.await.unwrap_err().is_cancelled());
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !a.stored().await.handoff_pause.is_paused() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let receipt = a.stored().await.handoff_pause.receipt().unwrap().clone();
    let recovery = runtime.recover_on_boot().await.unwrap();
    runtime.apply_recovery(recovery).await.unwrap();
    assert_eq!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            receipt: receipt.clone(),
            outcome: ReleaseOutcome::HealthyReleased
        }
    );
    assert_eq!(
        progress(&a, &id).await["source_receipt"],
        serde_json::to_value(&receipt).unwrap()
    );
    assert_eq!(
        progress(&a, &id).await["source_release"]["kind"],
        "healthy_released"
    );
    assert_eq!(b.stored().await.handoff_pause, PauseState::Unpaused);
    assert_eq!(active(&a).await, 0);
    a.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_coordinator_faulted_release_is_needs_recovery_not_healthy() {
    let mut a = Fixture::new().await;
    a.harness = PlannerHarness::run(super::handoff_pause_tests::params(&a, a.stored().await));
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = hook(&id, "SourcePaused");
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    super::handoff_pause_fault_tests::error(&a).await;
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    let CoordinatorResult::NeedsRecovery { reason } =
        coordinator::query(&runtime, &request).await.unwrap()
    else {
        panic!("fault cannot prove Healthy")
    };
    assert!(reason.contains("recovery"));
    assert_eq!(active(&a).await, 0);
    assert!(matches!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            outcome: ReleaseOutcome::ReleasedWithBackendFault { .. },
            ..
        }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    a.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_coordinator_release_storage_failure_reopens_original_progress_and_explicitly_retries()
 {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("coordinator.sqlite").display()
    );
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = hook(&id, "SourcePaused");
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    sqlx::query(
        r#"
            CREATE TRIGGER fail_release BEFORE UPDATE OF handle_state_json ON worker_sessions WHEN
            json_extract(NEW.handle_state_json,'$.handoff_pause.state')='released' BEGIN SELECT
            RAISE(ABORT,'release storage unavailable'); END
        "#,
    )
    .execute(a.repo.pool())
    .await
    .unwrap();
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    let original = a.stored().await.handoff_pause.receipt().unwrap().clone();
    assert_eq!(active(&a).await, 1);
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let runtime = runtime_with_repo(&a, reopened.clone(), registry);
    let OperationReceipt::AwaitingRetry { attempt, .. } =
        runtime.observe_operation(&id).await.unwrap()
    else {
        panic!("must retain retry")
    };
    let payload: String = sqlx::query_scalar("SELECT payload_json FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["source"],
        serde_json::to_value(original.request()).unwrap()
    );
    sqlx::query("DROP TRIGGER fail_release")
        .execute(reopened.pool())
        .await
        .unwrap();
    coordinator::retry(&runtime, &request, attempt)
        .await
        .unwrap();
    assert_eq!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            receipt: original,
            outcome: ReleaseOutcome::HealthyReleased
        }
    );
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_activation_hold_requires_explicit_original_generation() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = hook(&id, "Committed");
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    // The real provider owner holds authentication after durable cutover.
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "error".into(),
            params: serde_json::json!({"error":{"codexErrorInfo":"unauthorized"}}),
        });
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    let CoordinatorResult::Forward {
        cutover,
        activation,
    } = coordinator::query(&runtime, &request).await.unwrap()
    else {
        panic!("forward only")
    };
    assert!(matches!(activation, ActivationResult::Blocked { .. }));
    assert_eq!(active(&a).await, 1);
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        ..
    } = *cutover;
    let op = runtime
        .find_by_kind_and_idempotency("handoff-activation", &operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "account/login/completed".into(),
            params: serde_json::json!({"success":true,"error":null}),
        });
    coordinator::begin(&runtime, request.clone()).await.unwrap();
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    coordinator::retry_activation(&runtime, &request, op.attempt)
        .await
        .unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_checkpoint_storage_failure_keeps_journal_for_generation_retry() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
        .await
        .unwrap();
    let hook = hook(&id, "SourcePaused");
    let drive = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.drive().await.unwrap() })
    };
    entered(&hook).await;
    sqlx::query(
        r#"
            CREATE TRIGGER fail_coordinator_progress BEFORE UPDATE OF tx_output_json ON operations
            WHEN OLD.kind='handoff-coordinator' BEGIN SELECT
            RAISE(ABORT,'coordinator progress unavailable'); END
        "#,
    )
    .execute(a.repo.pool())
    .await
    .unwrap();
    coordinator::cancel(&runtime, &request).await.unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert_eq!(active(&a).await, 1);
    assert!(a.stored().await.handoff_pause.is_paused());
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::NeedsRecovery { .. }
    ));
    let original = progress(&a, &id).await;
    sqlx::query("DROP TRIGGER fail_coordinator_progress")
        .execute(a.repo.pool())
        .await
        .unwrap();
    let op = runtime
        .find_by_kind_and_idempotency("handoff-coordinator", &request.key)
        .await
        .unwrap()
        .unwrap();
    coordinator::retry(&runtime, &request, op.attempt)
        .await
        .unwrap();
    assert_eq!(
        progress(&a, &id).await["source_receipt"],
        original["source_receipt"]
    );
    assert!(matches!(
        a.stored().await.handoff_pause,
        PauseState::Released {
            outcome: ReleaseOutcome::HealthyReleased,
            ..
        }
    ));
    assert_eq!(active(&a).await, 0);
}

#[tokio::test]
async fn handoff_coordinator_system_notifications_survive_pause_cutover_and_activation_boundaries()
{
    for stage in ["SourcePaused", "CommitIntent", "Committed"] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, registry) = runtime(&a, &b);
        let request = request(&a, &b).await;
        let id = coordinator::admit_for_test(a.repo.pool(), request.clone())
            .await
            .unwrap();
        let hook = hook(&id, stage);
        let drive = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.drive().await.unwrap() })
        };
        entered(&hook).await;
        let event = Event::TrackWakeRequested {
            track_id: request.track_id.clone(),
            source: "handoff-boundary".into(),
            key: crate::model::new_id(),
            text: format!("handoff boundary {stage}"),
        };
        let (event_id, entry) = if stage == "Committed" {
            b.event_entry(&event).await
        } else {
            a.event_entry(&event).await
        };
        if stage != "Committed" {
            a.harness
                .observe_envelope(entry.observation(), event_id)
                .await
                .unwrap();
        }
        hook.release.notify_one();
        drive.await.unwrap();
        let CoordinatorResult::Forward { cutover, .. } =
            coordinator::query(&runtime, &request).await.unwrap()
        else {
            panic!("must forward")
        };
        let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
            planner_session_id,
            ..
        } = *cutover;
        let row = a
            .repo
            .session_get_by_id(&planner_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap();
        let snapshot = HarnessSnapshot::parse_known(row.handle_state_json.unwrap()).unwrap();
        assert_eq!(snapshot.push_watermark, event_id, "{stage}");
        let pending = serde_json::to_string(&snapshot).unwrap();
        let issued = serde_json::to_string(&a.daemon.started_turns_for_test()).unwrap();
        assert!(
            pending.contains(&format!("handoff boundary {stage}"))
                || issued.contains(&format!("handoff boundary {stage}")),
            "{stage}"
        );
        registry
            .get(&planner_session_id)
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}
