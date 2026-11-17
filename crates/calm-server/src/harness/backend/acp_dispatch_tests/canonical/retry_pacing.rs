//! Repeated BeforeReceipt refusal through the actual canonical scheduler.
use super::*;
use crate::harness::{HarnessConfig, HarnessRuntimeSnapshot, PlannerHarness, PlannerHarnessParams};
use calm_truth::session_repo::SessionRepo;

#[tokio::test]
async fn acp_planner_before_receipt_not_dispatched_backs_off() {
    let fx = Fixture::new("reply").await;
    // Closed ACP control refuses after receipt lookup proves absence, before any prompt.
    fx.session.shutdown().await.unwrap();
    let row = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    let frame: crate::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(row.handle_state_json.clone().unwrap()).unwrap();
    let write_db = fx.owner.repo.db().clone();
    let message = write_db
        .writer()
        .with_writer(|writer| {
            Box::pin(async {
                let mut tx = writer.begin_immediate().await.unwrap();
                let stored = submission_plan_tx(&mut tx, &fx.submission).await.unwrap();
                let AcceptedInputRef::Human { message } =
                    stored.plan.input_refs().unwrap().remove(0)
                else {
                    panic!("human")
                };
                tx.commit().await.unwrap();
                message
            })
        })
        .await
        .unwrap();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: fx.owner.worker.clone(),
        track_id: fx.owner.track.clone().into(),
        card_id: fx.owner.card.clone().into(),
        thread_id: Some(fx.owner.thread.clone()),
        repo: fx.owner.repo.clone(),
        events: crate::event::EventBus::new(),
        card_role_cache: crate::card_role_cache::CardRoleCache::new(),
        track_area_cache: crate::track_area_cache::TrackAreaCache::new(),
        backend: fx.backend.clone(),
        registry: fx.registry.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessRuntimeSnapshot::decode(serde_json::to_value(frame).unwrap()).unwrap(),
    })
    .await
    .unwrap();
    harness
        .observe_canonical_source(message, fx.source.clone())
        .await
        .unwrap();
    let attempts = || async {
        let row = fx
            .owner
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(
                fx.owner.worker.clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_value::<crate::harness::snapshot::CanonicalHarnessSnapshot>(
            row.handle_state_json.unwrap(),
        )
        .unwrap()
        .canonical_inputs
        .attempts
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while attempts().await.len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let reports = attempts().await;
    harness.shutdown().await.unwrap();
    assert_eq!(
        reports.len(),
        2,
        "second consecutive refusal must wait longer than the fixed two-second retry"
    );
    assert!(
        reports
            .iter()
            .all(|report| matches!(report.delivery, InputDeliveryFact::NotDispatched { .. }))
    );
    assert_eq!(fx.prompts(), 0);
}

#[tokio::test]
async fn acp_planner_issuing_turn_released_before_receipt_recovers_after_boot() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", true).await;
    fx.session.shutdown().await.unwrap();
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::NotDispatched(_)
    ));
    let before = runtime_frame_observation(fx.owner.repo.pool(), &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert_eq!(before.snapshot.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(before.snapshot.canonical_inputs.active.is_none());
    crate::acp_planner::config::recover_residents_after_boot(
        fx.owner.repo.as_ref(),
        fx.session.host(),
    )
    .await
    .unwrap();
    let after = runtime_frame_observation(fx.owner.repo.pool(), &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert_eq!(after.snapshot.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(
        after.snapshot.canonical_inputs,
        before.snapshot.canonical_inputs
    );
    assert_eq!(fx.prompts(), 0);
}
