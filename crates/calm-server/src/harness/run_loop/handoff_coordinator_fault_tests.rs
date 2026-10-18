//! Faults enter through the actual original user begin and file-backed operation runner.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::runtime_with_repo;
use super::handoff_coordinator_tests::{active, entered, request, runtime, start_at};
use super::*;
use crate::model::CardRole;
use crate::operation::handoff_activation::ActivationResult;
use crate::operation::handoff_coordinator::{self as handoff, CoordinatorResult};

#[tokio::test]
async fn handoff_coordinator_each_resource_storage_stage_reopens_and_forwards_or_retains_unknown() {
    for (stage, trigger, unknown, before) in [
        (
            "prepare",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF tx_output_json ON operations WHEN
        NEW.kind='handoff-workflow' AND
        json_extract(NEW.tx_output_json,'$.result.resources.state')='prepared' BEGIN SELECT
        RAISE(ABORT,'prepare fault'); END
        "#,
            false,
            0,
        ),
        (
            "intent",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF tx_output_json ON operations WHEN
        NEW.kind='handoff-workflow' AND
        json_extract(NEW.tx_output_json,'$.result.resources.state')='native_intent' BEGIN SELECT
        RAISE(ABORT,'intent fault'); END
        "#,
            false,
            0,
        ),
        (
            "bind",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF state ON worker_sessions WHEN
        OLD.state='pending_activation' AND NEW.state='starting' BEGIN SELECT RAISE(ABORT,'bind fault');
        END
        "#,
            true,
            1,
        ),
        (
            "install",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF handle_state_json ON worker_sessions WHEN
        OLD.state='starting' BEGIN SELECT RAISE(ABORT,'install fault'); END
        "#,
            false,
            1,
        ),
        (
            "terminal",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF tx_output_json ON operations WHEN
        NEW.kind='handoff-workflow' AND json_extract(NEW.tx_output_json,'$.result.state')='activated'
        BEGIN SELECT RAISE(ABORT,'terminal fault'); END
        "#,
            false,
            1,
        ),
        (
            "reservation",
            r#"
        CREATE TRIGGER fault BEFORE UPDATE OF active ON planner_handoff_coordinators WHEN OLD.active=1
        AND NEW.active=0 BEGIN SELECT RAISE(IGNORE); END
        "#,
            false,
            1,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("fault.db").display()
        );
        let a = Fixture::for_role_at(CardRole::Planner, &url).await;
        let b = a.sibling(CardRole::Assistant).await;
        let (original, registry) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        sqlx::query(trigger).execute(a.repo.pool()).await.unwrap();
        let id = handoff::begin(&original, approval.clone()).await.unwrap();
        let op = original
            .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.id, id);
        assert_eq!(active(&a).await, 1, "{stage}");
        assert_eq!(
            a.daemon.started_thread_params_for_test().len(),
            before,
            "{stage}"
        );
        let old_snapshot =
            super::handoff_activation_test_support::stored(&a, &approval.source_session_id).await;
        assert!(matches!(
            old_snapshot.handoff_pause,
            crate::harness::handoff_pause::PauseState::ConsumedByHandoff { .. }
        ));
        drop(original);
        let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
        sqlx::query("DROP TRIGGER fault")
            .execute(reopened.pool())
            .await
            .unwrap();
        let restored = runtime_with_repo(&a, reopened, registry.clone());
        restored
            .apply_recovery(restored.recover_on_boot().await.unwrap())
            .await
            .unwrap();
        // A stale generation cannot run a forward effect.
        handoff::retry(&restored, &approval, op.attempt + 1)
            .await
            .unwrap();
        assert_eq!(
            a.daemon.started_thread_params_for_test().len(),
            before,
            "{stage}"
        );
        handoff::retry(&restored, &approval, op.attempt)
            .await
            .unwrap();
        let CoordinatorResult::Forward {
            activation,
            cutover,
        } = handoff::query(&restored, &approval).await.unwrap()
        else {
            panic!("identity was committed: {stage}");
        };
        if unknown {
            assert!(
                matches!(activation, ActivationResult::Unknown { .. }),
                "{stage}: {activation:?}"
            );
            assert_eq!(active(&a).await, 1);
        } else {
            assert!(
                matches!(activation, ActivationResult::Activated { .. }),
                "{stage}: {activation:?}"
            );
            assert_eq!(active(&a).await, 0);
            let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation { planner_session_id,.. }=*cutover;
            registry
                .shutdown_and_remove(&planner_session_id)
                .await
                .unwrap();
        }
        assert_eq!(
            a.daemon.started_thread_params_for_test().len(),
            1,
            "{stage}"
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind LIKE 'handoff-%'")
                .fetch_one(a.repo.pool())
                .await
                .unwrap();
        assert_eq!(count, 1, "{stage}");
    }
}

#[tokio::test]
async fn handoff_coordinator_committed_reply_drop_and_runner_stuck_forward_original_identity() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let approval = request(&a, &b).await;
    let (id, hook, drive) = start_at(&runtime, &approval, "Committed").await;
    entered(&hook).await;
    drive.abort();
    assert!(drive.await.unwrap_err().is_cancelled());
    hook.release.notify_one();
    let CoordinatorResult::Forward { cutover, .. } =
        handoff::query(&runtime, &approval).await.unwrap()
    else {
        panic!("atomic committed identity");
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        planner_session_id,
        ..
    } = *cutover;
    assert_eq!(operation_id, id);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    runtime
        .apply_recovery(runtime.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    assert!(matches!(
        handoff::query(&runtime, &approval).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    registry
        .shutdown_and_remove(&planner_session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_coordinator_activated_runner_tail_stuck_and_successor_handoff_preserve_history() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let c = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    registry.insert(c.harness.inner.worker_session_id.clone(), c.harness.clone());
    let approval = request(&a, &b).await;
    sqlx::query(r#"
        CREATE TRIGGER tail_fault BEFORE UPDATE OF phase ON operations WHEN OLD.kind='handoff-workflow'
        AND NEW.phase='spawn_started' AND json_extract(OLD.tx_output_json,'$.result.state')='activated'
        BEGIN SELECT RAISE(ABORT,'runner tail fault after terminal'); END
        "#)
        .execute(a.repo.pool()).await.unwrap();
    handoff::begin(&runtime, approval.clone()).await.unwrap();
    let old = runtime
        .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(old.phase, crate::operation::Phase::Stuck { .. }));
    let CoordinatorResult::Forward {
        cutover,
        activation: ActivationResult::Activated { .. },
    } = handoff::query(&runtime, &approval).await.unwrap()
    else {
        panic!("runner failure cannot override business success");
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        planner_session_id,
        ..
    } = *cutover;
    let installed = registry.get(&planner_session_id).unwrap();
    let before = installed.snapshot().await;
    assert_eq!(active(&a).await, 0);
    sqlx::query("DROP TRIGGER tail_fault")
        .execute(a.repo.pool())
        .await
        .unwrap();
    // Finish the real first provider turn before the user makes another approval.
    // The new begin itself still refuses busy owners immediately.
    let first = super::handoff_activation_recovery_tests::running_snapshot(&installed, None).await;
    let thread = first.last_thread_id.unwrap();
    a.daemon.clear_active_turn_for_test(&thread);
    super::handoff_activation_recovery_tests::notification(&a,&installed,"turn/completed",serde_json::json!({"threadId":thread,"turn":{"id":first.last_turn_id.unwrap(),"status":"completed"}})).await;
    assert!(crate::harness::handoff_pause::PauseState::quiet_checkpoint(
        &installed.snapshot().await
    ));
    let mut successor = request(&b, &c).await;
    successor.source_session_id = planner_session_id.clone();
    let facts:Vec<(String,String,Option<String>,String)>=sqlx::query_as("SELECT c.id,c.role,c.session_id,w.state FROM cards c JOIN worker_sessions w ON w.id=c.session_id WHERE c.id IN (?1,?2)")
        .bind(successor.source_card_id.as_str()).bind(successor.target_card_id.as_str()).fetch_all(a.repo.pool()).await.unwrap();
    let root: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(successor.track_id.as_str())
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    handoff::begin(&runtime, successor.clone()).await.unwrap_or_else(|error|panic!("second approval failed: {error}; facts={facts:?}; root={root}; request={successor:?}"));
    assert!(matches!(
        handoff::query(&runtime, &successor).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    let current: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(approval.track_id.as_str())
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert_ne!(current, planner_session_id);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 2);
    assert!(matches!(
        handoff::retry(&runtime, &approval, old.attempt)
            .await
            .unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    handoff::begin(&runtime, approval.clone()).await.unwrap();
    assert!(
        matches!(handoff::query(&runtime,&approval).await.unwrap(),CoordinatorResult::Forward { activation:ActivationResult::Activated { session_id },.. } if session_id==planner_session_id)
    );
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 2);
    assert!(Arc::ptr_eq(
        &installed.inner,
        &registry.get(&planner_session_id).unwrap().inner
    ));
    assert_eq!(
        before.last_thread_id,
        installed.snapshot().await.last_thread_id
    );
    registry.shutdown_and_remove(&current).await.unwrap();
    installed.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_coordinator_legacy_reserved_or_paused_adoption_preserves_claim_and_fences_old_driver()
 {
    for paused in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let legacy = handoff::admit_for_test(a.repo.pool(), approval.clone())
            .await
            .unwrap();
        // Version-one fixture uses real admitted scope and, when present, an actual serial pause.
        // It contains no successful cutover, resource or terminal receipt.
        let receipt = if paused {
            Some(
                a.harness
                    .pause_for_handoff(super::handoff_pause_tests::request(&a))
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let progress = serde_json::json!({"stage":if paused { "source_paused" } else { "reserved" },"source_attempted":paused,"target_attempted":false,
            "source_receipt":receipt,"target_receipt":null,"source_release":null,"target_release":null,"plan":null,"committed":null,"reason":null});
        let mut input: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
                .bind(&legacy)
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        input.as_object_mut().unwrap().remove("version");
        if let Some(receipt) = &receipt {
            input["source"] = serde_json::to_value(receipt.request()).unwrap();
        }
        let output =
            crate::operation::TxOutput::new("track", Some(approval.track_id.to_string()), progress);
        sqlx::query(r#"UPDATE operations SET kind='handoff-coordinator',payload_json=?1,tx_output_json=?2,phase='stuck',phase_detail_json='{"reason":"Legacy handoff requires explicit adoption","since":0,"from_phase":"app_server_interact"}' WHERE id=?3"#)
            .bind(input.to_string()).bind(if paused { Some(serde_json::to_string(&output).unwrap()) } else { None }).bind(&legacy).execute(a.repo.pool()).await.unwrap();
        let before: (String, Option<String>) =
            sqlx::query_as("SELECT payload_json,tx_output_json FROM operations WHERE id=?1")
                .bind(&legacy)
                .fetch_one(a.repo.pool())
                .await
                .unwrap();
        assert_eq!(
            handoff::begin(&runtime, approval.clone()).await.unwrap(),
            legacy
        );
        assert!(handoff::retry(&runtime, &approval, 1).await.is_err());
        handoff::retry(&runtime, &approval, 0).await.unwrap();
        assert!(matches!(
            handoff::query(&runtime, &approval).await.unwrap(),
            CoordinatorResult::CancelledOrRefused
        ));
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
        assert_eq!(active(&a).await, 0);
        let after: (String, Option<String>) =
            sqlx::query_as("SELECT payload_json,tx_output_json FROM operations WHERE id=?1")
                .bind(&legacy)
                .fetch_one(a.repo.pool())
                .await
                .unwrap();
        assert_eq!(after, before);
        assert!(
            sqlx::query("UPDATE operations SET phase='succeeded' WHERE id=?1")
                .bind(&legacy)
                .execute(a.repo.pool())
                .await
                .is_err()
        );
        if let Some(receipt) = receipt {
            assert_eq!(
                a.stored().await.handoff_pause,
                crate::harness::handoff_pause::PauseState::Released {
                    receipt,
                    outcome: crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
                }
            );
        } else {
            assert_eq!(
                a.stored().await.handoff_pause,
                crate::harness::handoff_pause::PauseState::Unpaused
            );
        }
    }
}

#[tokio::test]
async fn handoff_coordinator_legacy_native_intent_adopts_unknown_without_reissuing() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = super::handoff_cutover_tests::plan(&a, &b).await;
    let cutover = super::handoff_activation_test_support::original(
        super::handoff_cutover_tests::commit(&a, plan)
            .await
            .unwrap(),
    );
    let child = crate::operation::handoff_activation::record_legacy_intent_for_test(
        a.repo.as_ref(),
        &cutover,
    )
    .await
    .unwrap();
    let before: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&child)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    let (runtime, _) = runtime(&a, &b);
    let receipt = crate::operation::handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    assert!(matches!(
        super::handoff_activation_test_support::result(&receipt),
        ActivationResult::Unknown { .. }
    ));
    let crate::operation::OperationReceipt::AwaitingRetry { attempt, .. } = receipt else {
        panic!("unknown keeps its original owner");
    };
    crate::operation::handoff_activation::retry(&runtime, cutover.clone(), attempt)
        .await
        .unwrap();
    assert!(matches!(
        crate::operation::handoff_activation::query(&runtime, &cutover)
            .await
            .unwrap(),
        ActivationResult::Unknown { .. }
    ));
    assert_eq!(active(&a).await, 1);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    assert!(
        sqlx::query("UPDATE operations SET tx_output_json=NULL WHERE id=?1")
            .bind(&child)
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    let after: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&child)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn handoff_coordinator_legacy_committed_identity_survives_old_runner_stuck() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = super::handoff_cutover_tests::plan(&a, &b).await;
    let retired = plan.scope.target_session_id.clone();
    let cutover = super::handoff_activation_test_support::original(
        super::handoff_cutover_tests::commit(&a, plan)
            .await
            .unwrap(),
    );
    // The identity/result are from the real SQL helper; only the lost runner tail is injected.
    sqlx::query(r#"UPDATE operations SET phase='stuck',phase_detail_json='{"reason":"legacy runner tail lost","since":0,"from_phase":"tx_committed"}' WHERE id=?1"#)
        .bind(&cutover).execute(a.repo.pool()).await.unwrap();
    let original: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
        .bind(&cutover)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1")
            .bind(retired)
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    let (runtime, registry) = runtime(&a, &b);
    let receipt = crate::operation::handoff_activation::activate(&runtime, cutover.clone())
        .await
        .unwrap();
    let ActivationResult::Activated { session_id } =
        super::handoff_activation_test_support::result(&receipt)
    else {
        panic!("committed identity requires a reachable forward owner");
    };
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    assert_eq!(active(&a).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT tx_output_json FROM operations WHERE id=?1")
            .bind(&cutover)
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        original
    );
    registry.shutdown_and_remove(&session_id).await.unwrap();
}
