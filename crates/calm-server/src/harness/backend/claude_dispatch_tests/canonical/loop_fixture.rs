//! Existing live-loop setup shared by the canonical Claude process tests.
use super::*;

impl Fixture {
    pub(super) async fn start_loop(&self) -> crate::harness::PlannerHarness {
        self.start_loop_in(Default::default()).await
    }
    pub(super) async fn start_loop_in(
        &self,
        registry: crate::harness::registry::HarnessRegistry,
    ) -> crate::harness::PlannerHarness {
        use crate::harness::{HarnessRuntimeSnapshot, PlannerHarness, PlannerHarnessParams};
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &self.owner.card, &self.owner.worker)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header["phase"] = serde_json::json!("issuing_turn");
        let written = persist_runtime_frame_tx(
            &mut tx,
            &self.owner.card,
            &self.owner.worker,
            &fence,
            header,
        )
        .await
        .unwrap();
        let refs = submission_plan_tx(&mut tx, &self.submission)
            .await
            .unwrap()
            .plan
            .input_refs()
            .unwrap();
        let [AcceptedInputRef::Human { message }] = refs.as_slice() else {
            panic!("human")
        };
        tx.commit().await.unwrap();
        let roles = crate::card_role_cache::CardRoleCache::new();
        roles.seed_from_db(self.owner.repo.pool()).await.unwrap();
        let areas = crate::track_area_cache::TrackAreaCache::new();
        areas.seed_from_db(self.owner.repo.pool()).await.unwrap();
        let harness = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: Default::default(),
            worker_session_id: self.owner.worker.clone(),
            track_id: crate::ids::TrackId(self.owner.track.clone()),
            card_id: crate::ids::CardId(self.owner.card.clone()),
            thread_id: Some(self.owner.thread.clone()),
            repo: self.owner.repo.clone(),
            events: crate::event::EventBus::new(),
            card_role_cache: roles,
            track_area_cache: areas,
            backend: self.backend.clone(),
            registry: registry.clone(),
            config: crate::harness::HarnessConfig::default(),
            snapshot: HarnessRuntimeSnapshot::decode(written.snapshot).unwrap(),
        })
        .await
        .unwrap();
        assert!(
            registry
                .try_reserve(self.owner.worker.clone())
                .unwrap()
                .install(harness.clone())
        );
        harness
            .observe_canonical_source(message.clone(), self.source.clone())
            .await
            .unwrap();
        harness
    }
    pub(super) async fn frame(&self) -> serde_json::Value {
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let carrier = current_input_fact_carrier_tx(&mut tx, &self.owner.card, &self.owner.worker)
            .await
            .unwrap();
        let frame = carrier.snapshot().clone();
        tx.commit().await.unwrap();
        frame
    }
    pub(super) async fn phase(&self, expected: &str) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let frame = self.frame().await;
                if frame["phase"] == expected {
                    return frame;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
