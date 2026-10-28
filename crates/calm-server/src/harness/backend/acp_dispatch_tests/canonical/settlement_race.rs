//! An absent maintenance driver has two settlers: the loop observation and the
//! owner's loss. Whichever commits second must accept the identical settlement.
use super::*;
use crate::harness::PlannerHarness;
use crate::test_seams::{
    MAINTENANCE_LOST_SETTLING, MAINTENANCE_OBSERVE_DECIDED, PausePoint, install_pause_for_test,
};
use serde_json::{Value, json};

struct Race {
    fx: Fixture,
    harness: PlannerHarness,
    _registry: crate::harness::HarnessRegistry,
    observe: PausePoint,
    operation: String,
    turn: String,
}

impl Race {
    /// Install a started maintenance frame whose driver is absent, with the
    /// loop observation armed to pause after deciding to wedge it.
    async fn new() -> Self {
        let fx = Fixture::new("unused").await;
        let (harness, registry) = super::compact::registered_recovery_harness(&fx).await;
        let observe = arm(&fx, MAINTENANCE_OBSERVE_DECIDED);
        let (operation, turn) = (crate::model::new_id(), crate::model::new_id());
        let mut frame = frame(&fx).await;
        frame["phase"] = json!("compacting");
        frame["compact_outcome"] = json!({
            "operation_id": operation, "thread_id": fx.owner.thread,
            "turn_id": turn, "status": "started"
        });
        store(&fx, frame).await;
        Self {
            fx,
            harness,
            _registry: registry,
            observe,
            operation,
            turn,
        }
    }

    fn lose(&self) -> tokio::task::JoinHandle<crate::error::Result<()>> {
        let (harness, turn) = (self.harness.clone(), self.turn.clone());
        tokio::spawn(async move {
            harness
                .complete_provider_execution(
                    &workspace_runtime::execution::ExecutionId::fresh().unwrap(),
                    &turn,
                    &crate::provider_execution::Purpose::Compaction {
                        turn_id: turn.clone(),
                    },
                    Value::Null,
                )
                .await
        })
    }

    async fn wait_wedged(&self) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while frame(&self.fx).await["phase"] != "wedged" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a settlement must commit the wedge");
    }

    fn assert_settled_once(&self, after: &Value) {
        assert_eq!(after["phase"], "wedged");
        assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
        assert_eq!(after["compact_outcome"]["operation_id"], self.operation);
        assert_eq!(after["compact_outcome"]["turn_id"], self.turn);
        assert_eq!(
            self.fx
                .requests()
                .iter()
                .filter(|frame| frame["method"] == "session/prompt"
                    && frame["params"]["prompt"] == json!([{"type":"text","text":"/compact"}]))
                .count(),
            0,
            "lost maintenance is never resubmitted"
        );
    }
}

fn arm(fx: &Fixture, point: &str) -> PausePoint {
    let pause = PausePoint::default();
    install_pause_for_test(point, &fx.owner.card, pause.clone());
    pause
}

async fn frame(fx: &Fixture) -> Value {
    fx.owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap()
}

async fn store(fx: &Fixture, frame: Value) {
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    crate::db::sqlite::session_set_handle_state_tx(&mut tx, &fx.owner.worker, Some(frame))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn acp_compact_absent_driver_loss_after_observed_wedge_is_idempotent() {
    let race = Race::new().await;
    let lost = arm(&race.fx, MAINTENANCE_LOST_SETTLING);
    race.observe.entered.notified().await;
    let loss = race.lose();
    lost.entered.notified().await;
    race.observe.release.notify_one();
    race.wait_wedged().await;
    lost.release.notify_one();
    let result = loss.await.unwrap();
    let after = frame(&race.fx).await;
    race.harness.shutdown().await.unwrap();
    result.expect("loss after the observed wedge is the same settlement");
    race.assert_settled_once(&after);
}

#[tokio::test]
async fn acp_compact_absent_driver_observation_after_loss_does_not_publish_issuance_conflict() {
    let race = Race::new().await;
    race.observe.entered.notified().await;
    race.lose().await.unwrap().unwrap();
    race.observe.release.notify_one();
    // Shutdown joins the loop, so the paused tick has published any error.
    race.harness.shutdown().await.unwrap();
    assert_eq!(race.harness.issuance_block().await, None);
    race.assert_settled_once(&frame(&race.fx).await);
}

#[tokio::test]
async fn acp_compact_foreign_settlement_still_conflicts() {
    let race = Race::new().await;
    race.observe.entered.notified().await;
    let mut foreign = frame(&race.fx).await;
    foreign["phase"] = json!("wedged");
    foreign["wedged_reason"] = json!("a foreign settlement");
    foreign["compact_outcome"]["status"] = json!("unconfirmed");
    store(&race.fx, foreign).await;
    race.observe.release.notify_one();
    race.harness.shutdown().await.unwrap();
    assert!(
        race.harness.issuance_block().await.is_some(),
        "a different concurrent settlement must keep the frame conflict"
    );
    let after = frame(&race.fx).await;
    assert_eq!(after["wedged_reason"], "a foreign settlement");
    assert_eq!(after["compact_outcome"]["operation_id"], race.operation);
}
