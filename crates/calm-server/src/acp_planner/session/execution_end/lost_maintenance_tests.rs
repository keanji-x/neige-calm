//! Exercise the ACP owner's retained-slot classification, including Turn slots.
use super::*;
use crate::harness::backend::MaintenanceTarget;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Cleanup,
    Historical,
    Canonical,
    Maintenance,
}

async fn retained(
    kind: Kind,
    identified: bool,
) -> (
    AcpPlannerSession,
    workspace_runtime::execution::ExecutionId,
    String,
    MaintenanceTarget,
) {
    let (session, _repo, thread) = super::super::admission_tests::fixture().await;
    let id = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let turn = crate::model::new_id();
    let params = &session.shared.params;
    let target = MaintenanceTarget {
        card_id: params.card_id.clone(),
        track_id: params.track_id.clone(),
        worker_session_id: params.worker_session_id.clone(),
        thread_id: thread,
        operation_id: crate::model::new_id(),
    };
    let input = match kind {
        Kind::Cleanup => DriverInput::Cleanup,
        Kind::Historical => DriverInput::Historical,
        Kind::Canonical => DriverInput::Canonical(Box::new(CanonicalDriver {
            receipt: AcpSubmission {
                worker_session_id: params.worker_session_id.clone(),
                client_id: "unused".into(),
                turn_id: turn.clone(),
                thread_id: target.thread_id.clone(),
                native_session_id: "native".into(),
                input_json: "{}".into(),
                state: AcpSubmissionState::Completed,
                outcome_json: None,
                kept_cleanup: None,
            },
            association: AcpCanonicalInput {
                logical_turn_id: calm_types::conversation::LogicalTurnId::parse(&crate::model::new_id()).unwrap(),
                proof: serde_json::from_value(json!({
                    "version":"v2","attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
                    "input_refs":[{"kind":"system","observation_id":"33333333333333333333333333333333"}],
                    "render_plan":{"version":"v1","submission_id":"11111111111111111111111111111111","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
                })).unwrap(),
            },
        })),
        Kind::Maintenance => DriverInput::Maintenance {
            target: target.clone(), turn: turn.clone(), native: "native".into(),
        },
    };
    install(
        &session.shared,
        DriverSlot {
            execution_id: identified.then(|| id.clone()),
            completion: Completion::RecoveredCompleted,
            input,
        },
    );
    (session, id, turn, target)
}

#[tokio::test]
async fn acp_compact_lost_target_stale_identity_refuses_every_slot_kind() {
    for kind in [
        Kind::Cleanup,
        Kind::Historical,
        Kind::Canonical,
        Kind::Maintenance,
    ] {
        let (session, _id, turn, _) = retained(kind, true).await;
        let before = slot(&session.shared).unwrap();
        let stale = workspace_runtime::execution::ExecutionId::fresh().unwrap();
        assert!(
            session
                .lost_maintenance_target(&stale, &turn)
                .await
                .is_err(),
            "{kind:?}"
        );
        assert!(Arc::ptr_eq(&before, &slot(&session.shared).unwrap()));
    }
}

#[tokio::test]
async fn acp_compact_lost_target_unidentified_slot_is_not_restart_absence() {
    for kind in [
        Kind::Cleanup,
        Kind::Historical,
        Kind::Canonical,
        Kind::Maintenance,
    ] {
        let (session, id, turn, _) = retained(kind, false).await;
        assert!(
            session.lost_maintenance_target(&id, &turn).await.is_err(),
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn acp_compact_lost_target_same_execution_uses_retained_operation() {
    let (session, id, turn, target) = retained(Kind::Maintenance, true).await;
    assert_eq!(
        session.lost_maintenance_target(&id, &turn).await.unwrap(),
        Some(target)
    );
}

#[tokio::test]
async fn acp_compact_lost_target_same_execution_wrong_turn_refuses() {
    let (session, id, _, _) = retained(Kind::Maintenance, true).await;
    assert!(
        session
            .lost_maintenance_target(&id, "other-turn")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn acp_compact_lost_target_same_execution_targetless_uses_recovery() {
    for kind in [Kind::Cleanup, Kind::Historical, Kind::Canonical] {
        let (session, id, turn, _) = retained(kind, true).await;
        assert_eq!(
            session.lost_maintenance_target(&id, &turn).await.unwrap(),
            None,
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn acp_compact_lost_target_busy_slot_refuses_without_waiting() {
    let (session, id, turn, _) = retained(Kind::Maintenance, true).await;
    let retained = slot(&session.shared).unwrap();
    let busy = retained.lock().await;
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        session.lost_maintenance_target(&id, &turn),
    )
    .await;
    drop(busy);
    assert!(
        result.is_ok(),
        "loss admission must not wait for a pending join"
    );
    assert!(result.unwrap().is_err());
    assert!(Arc::ptr_eq(&retained, &slot(&session.shared).unwrap()));
}

#[tokio::test]
async fn acp_compact_lost_target_live_driver_refuses_every_slot_kind() {
    for kind in [
        Kind::Cleanup,
        Kind::Historical,
        Kind::Canonical,
        Kind::Maintenance,
    ] {
        let (session, id, turn, _) = retained(kind, true).await;
        let retained = slot(&session.shared).unwrap();
        let handle = tokio::spawn(std::future::pending());
        let abort = handle.abort_handle();
        retained.lock().await.completion = Completion::Driver(RetainedJoin::new(handle));
        let result = session.lost_maintenance_target(&id, &turn).await;
        assert!(
            result.is_err_and(|error| error.to_string().contains("remains live")),
            "{kind:?}"
        );
        assert!(Arc::ptr_eq(&retained, &slot(&session.shared).unwrap()));
        abort.abort();
    }
}
