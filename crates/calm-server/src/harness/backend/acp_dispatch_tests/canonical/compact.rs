//! Retrying the backend operation itself cannot write a second native prompt.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
use crate::harness::native_admission::NativePromptAdmission;
use calm_truth::session_repo::SessionRepo;
use provider::control::CompactSubmission;
use serde_json::json;

#[tokio::test]
async fn acp_compact_retained_operation_never_sends_second_prompt() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    std::fs::write(fx.dir.path().join("scenario"), "compact-lost").unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let request = || CompactRequest {
        selection: TurnModelSelection::inherit(),
    };
    assert!(matches!(
        fx.backend
            .compact_canonical(target.clone(), request(), &admission)
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(
                fx.backend.observe_maintenance(&target).await.unwrap(),
                Some(MaintenanceObservation::Turn {
                    settled: Some(false),
                    ..
                })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let count = || {
        fx.requests()
            .iter()
            .filter(|frame| {
                frame["method"] == "session/prompt"
                    && frame["params"]["prompt"] == json!([{"type":"text","text":"/compact"}])
            })
            .count()
    };
    assert_eq!(count(), 1);
    let repeated = fx
        .backend
        .compact_canonical(target, request(), &admission)
        .await
        .unwrap();
    fx.wait_idle().await;
    assert_eq!(
        count(),
        1,
        "an uncertain operation must never send /compact twice"
    );
    assert!(matches!(repeated, CompactSubmission::Unconfirmed { .. }));
    fx.session.shutdown().await.unwrap();
}

#[derive(Clone, Copy, Debug)]
enum LossCase {
    Stale,
    Live,
    Busy,
    Failed { issue_next: bool },
}

#[tokio::test]
async fn acp_compact_stale_recovery_does_not_wedge_successor_same_turn() {
    retained_loss(LossCase::Stale).await;
}

#[tokio::test]
async fn acp_compact_live_recovery_refuses_before_loss_projection() {
    retained_loss(LossCase::Live).await;
}

#[tokio::test]
async fn acp_compact_pending_join_refuses_before_loss_projection() {
    retained_loss(LossCase::Busy).await;
}

#[tokio::test]
async fn acp_compact_failed_join_recovery_allows_issuance_and_shutdown() {
    for issue_next in [false, true] {
        retained_loss(LossCase::Failed { issue_next }).await;
    }
}

async fn retained_loss(case: LossCase) {
    let started = std::time::Instant::now();
    let phase = |label: &str| eprintln!("loss {case:?} +{:?}: {label}", started.elapsed());
    phase("fixture start");
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    phase("initial dispatched");
    fx.wait_end().await;
    phase("initial END consumed");
    let (harness, _registry) = registered_recovery_harness(&fx).await;
    std::fs::write(fx.dir.path().join("scenario"), "compact-no-response").unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target.clone(),
                CompactRequest {
                    selection: TurnModelSelection::inherit()
                },
                &admission
            )
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    let claim =
        calm_truth::db::sqlite::provider_execution::load(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
            .unwrap();
    let crate::provider_execution::Purpose::Compaction { turn_id } = &claim.record().purpose else {
        panic!("maintenance");
    };
    let row = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    let mut frame = row.handle_state_json.unwrap();
    // Issuance is an owner-layer check with no installed maintenance frame.
    // The separate framed case must retain Wedged/Unconfirmed and allow shutdown;
    // that canonical state deliberately refuses new Initial claims.
    if !matches!(case, LossCase::Failed { issue_next: true }) {
        frame["phase"] = json!("compacting");
        frame["compact_outcome"] = json!({"operation_id":target.operation_id,"thread_id":target.thread_id,"turn_id":turn_id,"status":"started"});
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        crate::db::sqlite::session_set_handle_state_tx(
            &mut tx,
            &fx.owner.worker,
            Some(frame.clone()),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert!(token.is_some());
    if matches!(case, LossCase::Failed { .. }) {
        phase("abort maintenance");
        fx.session.abort_driver_for_test().await;
        fx.wait_driver_finished().await;
        phase("maintenance join finished");
    }
    let stale_id = if matches!(case, LossCase::Stale) {
        workspace_runtime::execution::ExecutionId::fresh().unwrap()
    } else {
        crate::provider_execution::execution_id(claim.record()).unwrap()
    };
    let busy = if matches!(case, LossCase::Busy) {
        Some(fx.session.lock_driver_for_test().await)
    } else {
        None
    };
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        harness.complete_provider_execution(
            &stale_id,
            turn_id,
            &crate::provider_execution::Purpose::Compaction {
                turn_id: turn_id.clone(),
            },
            serde_json::Value::Null,
        ),
    )
    .await;
    drop(busy);
    let result = result.expect("loss admission must not wait for a pending join");
    let after = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap();
    if matches!(case, LossCase::Failed { .. }) {
        result.unwrap();
        phase("loss projected and join retired");
        if matches!(case, LossCase::Failed { issue_next: false }) {
            assert_eq!(after["phase"], "wedged");
            assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
            assert_eq!(
                after["compact_outcome"]["operation_id"],
                target.operation_id
            );
        } else {
            assert_eq!(
                after, frame,
                "owner cleanup must not invent a maintenance frame"
            );
        }
        assert!(matches!(
            fx.backend.observe_maintenance(&target).await.unwrap(),
            Some(MaintenanceObservation::Unconfirmed)
        ));
        if matches!(case, LossCase::Failed { issue_next: false }) {
            harness.shutdown().await.unwrap();
            phase("framed shutdown complete");
            return;
        }
        // The owner recovers/clears the claim through the production admission
        // path. A fresh input must not encounter the old cached failed join.
        let host = fx.session.host();
        let engine = host.execution.engine().unwrap();
        host.provider_execution
            .stop(
                &fx.owner.repo.sqlite_db().unwrap(),
                Some(engine),
                &fx.owner.worker,
                &claim,
                crate::provider_execution::StopReason::Shutdown,
            )
            .await
            .unwrap();
        crate::provider_execution::wait_settled(
            engine,
            &crate::provider_execution::execution_id(claim.record()).unwrap(),
            Duration::from_secs(12),
        )
        .await
        .unwrap();
        fx.backend.provider_transition_gate().await.unwrap();
        phase("claim settled and transition gate passed");
        std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
        let successor = fx.claim_successor().await;
        let report = fx.dispatch_submission(&successor).await;
        let successor_turn = report.report.outcome().as_ref().unwrap().turn_id();
        phase("successor dispatched");
        // The registered harness owns END consumption. Competing with it via
        // fx.wait_end() can miss the one-shot Some(frame) forever. Synchronize
        // on the durable END for this successor, regardless of when it commits.
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let header = harness.runtime_header().await.unwrap();
                if header.phase == crate::harness::snapshot::HarnessPhaseTag::TurnCompleted
                    && header.last_turn_id.as_deref() == Some(successor_turn.as_str())
                    && header.last_thread_id.as_deref() == Some(fx.owner.thread.as_str())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("harness must commit the successor END");
        phase("harness committed successor END");
        assert!(
            fx.complete().await.is_none(),
            "END is consumed exactly once"
        );
        harness.shutdown().await.unwrap();
        phase("successor shutdown complete");
        let compact_prompts = fx
            .requests()
            .iter()
            .filter(|frame| {
                frame["method"] == "session/prompt"
                    && frame["params"]["prompt"] == json!([{"type":"text","text":"/compact"}])
            })
            .count();
        assert_eq!(compact_prompts, 1, "lost maintenance is never resent");
        return;
    }
    let retained =
        calm_truth::db::sqlite::provider_execution::load(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap();
    let after_token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    let active = fx.session.active_turn_id_for_thread(&fx.owner.thread);
    let retained_target = fx
        .backend
        .lost_maintenance_target(
            &crate::provider_execution::execution_id(claim.record()).unwrap(),
            turn_id,
        )
        .await;
    fx.session.abort_driver_for_test().await;
    let _ = harness.shutdown().await;
    assert_eq!(retained, Some(claim.clone()));
    assert_eq!(after_token, token);
    assert_eq!(active.as_deref(), Some(turn_id.as_str()));
    assert_eq!(
        after, frame,
        "refused loss must preserve the entire maintenance frame"
    );
    assert!(retained_target.is_err(), "a live slot cannot admit loss");
    assert!(
        result.as_ref().is_err_and(|e| e.to_string().contains(
            if matches!(case, LossCase::Stale) {
                "identity changed"
            } else if matches!(case, LossCase::Busy) {
                "join is pending"
            } else {
                "remains live"
            }
        )),
        "backend must refuse loss before projection: {result:?}"
    );
}

async fn recovery_harness(fx: &Fixture) -> crate::harness::PlannerHarness {
    registered_recovery_harness(fx).await.0
}

async fn registered_recovery_harness(
    fx: &Fixture,
) -> (
    crate::harness::PlannerHarness,
    crate::harness::HarnessRegistry,
) {
    use crate::harness::{
        HarnessConfig, HarnessRuntimeSnapshot, PlannerHarness, PlannerHarnessParams,
    };
    let row = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    let roles = crate::card_role_cache::CardRoleCache::new();
    roles.seed_from_db(fx.owner.repo.pool()).await.unwrap();
    let areas = crate::track_area_cache::TrackAreaCache::new();
    areas.seed_from_db(fx.owner.repo.pool()).await.unwrap();
    let registry = crate::harness::HarnessRegistry::default();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: fx.owner.worker.clone(),
        track_id: crate::ids::TrackId(fx.owner.track.clone()),
        card_id: crate::ids::CardId(fx.owner.card.clone()),
        thread_id: Some(fx.owner.thread.clone()),
        repo: fx.owner.repo.clone(),
        events: crate::event::EventBus::new(),
        card_role_cache: roles,
        track_area_cache: areas,
        backend: fx.backend.clone(),
        registry: registry.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessRuntimeSnapshot::decode(row.handle_state_json.unwrap()).unwrap(),
    })
    .await
    .unwrap();
    registry.insert(fx.owner.worker.clone(), harness.clone());
    (harness, registry)
}

#[tokio::test]
async fn acp_compact_absent_driver_restart_still_projects_unconfirmed() {
    let fx = Fixture::new("unused").await;
    let harness = recovery_harness(&fx).await;
    let mut frame = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap();
    let turn = crate::model::new_id();
    frame["phase"] = json!("compacting");
    frame["compact_outcome"] = json!({
        "operation_id": crate::model::new_id(), "thread_id": fx.owner.thread,
        "turn_id": turn, "status": "started"
    });
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    crate::db::sqlite::session_set_handle_state_tx(&mut tx, &fx.owner.worker, Some(frame))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    harness
        .complete_provider_execution(
            &workspace_runtime::execution::ExecutionId::fresh().unwrap(),
            &turn,
            &crate::provider_execution::Purpose::Compaction {
                turn_id: turn.clone(),
            },
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let after = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap();
    harness.shutdown().await.unwrap();
    assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
}

#[tokio::test]
async fn acp_compact_failed_setup_cleanup_claim_can_recover() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let (harness, _registry) = registered_recovery_harness(&fx).await;
    std::fs::write(fx.dir.path().join("scenario"), "wrong-identity").unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER prevent_clear BEFORE UPDATE OF provider_execution_json ON worker_sessions ",
        "WHEN NEW.provider_execution_json IS NULL AND OLD.provider_execution_json IS NOT NULL ",
        "BEGIN SELECT RAISE(FAIL,'retry owner completion'); END",
    ))
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let submitted = fx
        .backend
        .compact_canonical(
            target,
            CompactRequest {
                selection: TurnModelSelection::inherit(),
            },
            &admission,
        )
        .await
        .unwrap();
    assert!(
        !matches!(submitted, CompactSubmission::Submitted { .. }),
        "must fail setup: {submitted:?}"
    );
    fx.wait_idle().await;
    let before =
        calm_truth::db::sqlite::provider_execution::load(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap();
    assert!(
        before.is_some(),
        "claim retained after transient clear failure"
    );
    sqlx::query("DROP TRIGGER prevent_clear")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let recovered = fx.backend.provider_transition_gate().await;
    let retained =
        calm_truth::db::sqlite::provider_execution::load(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap();
    let _ = harness.shutdown().await;
    assert!(
        recovered.is_ok(),
        "same-execution cleanup is not a successor mismatch: {recovered:?}; retained={retained:?}"
    );
    assert!(retained.is_none());
}
