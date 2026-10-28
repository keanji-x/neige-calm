//! An absent maintenance driver has two settlers: the loop observation and the
//! owner's loss. Whichever commits second must accept the identical settlement.
use super::*;
use crate::harness::PlannerHarness;
use crate::test_seams::{
    MAINTENANCE_LOST_SETTLING, MAINTENANCE_OBSERVE_DECIDED, PausePoint, install_pause_for_test,
};
use calm_truth::session_repo::SessionRepo;
use serde_json::{Value, json};

/// Every wait in a race is bounded, so a seam or lock deadlock fails fast.
async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

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
        let fx = within("fixture", Fixture::new("unused")).await;
        within("retire fixture claim", neutralize(&fx)).await;
        let (harness, registry) =
            within("harness", super::compact::registered_recovery_harness(&fx)).await;
        let observe = arm(&fx, MAINTENANCE_OBSERVE_DECIDED);
        let (operation, turn) = (crate::model::new_id(), crate::model::new_id());
        let mut frame = within("frame", frame(&fx)).await;
        frame["phase"] = json!("compacting");
        frame["compact_outcome"] = json!({
            "operation_id": operation, "thread_id": fx.owner.thread,
            "turn_id": turn, "status": "started"
        });
        within("store", store(&fx, frame)).await;
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
        within("a settlement to commit the wedge", async {
            while frame(&self.fx).await["phase"] != "wedged" {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    async fn shutdown(&self) {
        within("harness shutdown", self.harness.shutdown())
            .await
            .unwrap();
    }

    fn assert_settled_once(&self, after: &Value) {
        assert_eq!(after["phase"], "wedged");
        assert_eq!(after["compact_outcome"]["status"], "unconfirmed");
        assert_eq!(after["compact_outcome"]["operation_id"], self.operation);
        assert_eq!(after["compact_outcome"]["turn_id"], self.turn);
        assert!(
            !self.fx.dir.path().join("requests.jsonl").exists(),
            "lost maintenance is never resubmitted"
        );
    }
}

fn arm(fx: &Fixture, point: &str) -> PausePoint {
    let pause = PausePoint::default();
    install_pause_for_test(point, &fx.owner.card, pause.clone());
    pause
}

async fn neutralize(fx: &Fixture) {
    // Fixture starts with an unattempted Initial claim. Retire it through the
    // repository's confirmed non-delivery path before any loop tick can resume
    // it without a live source and leave an unrelated issuance block behind.
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let attempt = InputAttempt {
        submission_id: fx.submission.clone(),
        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
    };
    let correlation = InputCorrelationFact {
        provider: fx.provider.clone(),
        thread_id: fx.owner.thread.clone(),
        native_session_id: None,
        local_turn_id: None,
        native_turn_id: None,
        rpc_request_id: None,
    };
    begin_input_attempt_tx(
        &mut tx,
        &mut carrier,
        attempt.clone(),
        correlation.clone(),
        FrozenInputInvocation {
            model: None,
            effort: None,
            permission: PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: attempt.clone(),
            correlation,
            delivery: InputDeliveryFact::NotDispatched {
                basis: "unused race fixture claim".into(),
            },
        },
    )
    .await
    .unwrap();
    release_submission_tx(&mut tx, &mut carrier, &attempt)
        .await
        .unwrap();
    // Keep startup neutral, without the released pending input. Compacting must
    // be installed only after startup: load_current would wedge it on restart
    // and bypass the loop observation seam this race exercises.
    let mut neutral = crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
    neutral.header.last_thread_id = Some(fx.owner.thread.clone());
    crate::db::sqlite::session_set_handle_state_tx(
        &mut tx,
        &fx.owner.worker,
        Some(serde_json::to_value(neutral).unwrap()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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
    // The loss pauses before its read/decide/write transaction, so the
    // observation's wedge commits first and the loss must read and accept it.
    let race = Race::new().await;
    let lost = arm(&race.fx, MAINTENANCE_LOST_SETTLING);
    within("observation decision", race.observe.entered.notified()).await;
    assert_eq!(
        within(
            "issuance block at observation",
            race.harness.issuance_block()
        )
        .await,
        None
    );
    let loss = race.lose();
    within("loss settlement seam", lost.entered.notified()).await;
    race.observe.release.notify_one();
    race.wait_wedged().await;
    lost.release.notify_one();
    let result = within("loss", loss).await.unwrap();
    let after = within("frame", frame(&race.fx)).await;
    race.shutdown().await;
    result.expect("loss after the observed wedge is the same settlement");
    race.assert_settled_once(&after);
}

#[tokio::test]
async fn acp_compact_absent_driver_observation_after_loss_does_not_publish_issuance_conflict() {
    let race = Race::new().await;
    within("observation decision", race.observe.entered.notified()).await;
    assert_eq!(
        within(
            "issuance block at observation",
            race.harness.issuance_block()
        )
        .await,
        None
    );
    within("loss", race.lose()).await.unwrap().unwrap();
    race.observe.release.notify_one();
    // Shutdown joins the loop, so the paused tick has published any error.
    race.shutdown().await;
    assert_eq!(
        within("issuance block", race.harness.issuance_block()).await,
        None
    );
    race.assert_settled_once(&within("frame", frame(&race.fx)).await);
}

#[tokio::test]
async fn acp_compact_foreign_settlement_still_conflicts() {
    let race = Race::new().await;
    within("observation decision", race.observe.entered.notified()).await;
    assert_eq!(
        within(
            "issuance block at observation",
            race.harness.issuance_block()
        )
        .await,
        None
    );
    let mut foreign = within("frame", frame(&race.fx)).await;
    foreign["phase"] = json!("wedged");
    foreign["wedged_reason"] = json!("a foreign settlement");
    foreign["compact_outcome"]["status"] = json!("unconfirmed");
    within("store", store(&race.fx, foreign)).await;
    race.observe.release.notify_one();
    race.shutdown().await;
    assert!(
        within("issuance block", race.harness.issuance_block())
            .await
            .is_some(),
        "a different concurrent settlement must keep the frame conflict"
    );
    let after = within("frame", frame(&race.fx)).await;
    assert_eq!(after["wedged_reason"], "a foreign settlement");
    assert_eq!(after["compact_outcome"]["operation_id"], race.operation);
}
