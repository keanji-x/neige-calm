//! Returned provider evidence remains recordable after the production failure
//! writer closes execution authority. No test manufactures a receipt or END.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::{connect, notify_thread, request, send};
use calm_truth::db::RepoRead;
use serde_json::json;

async fn real() -> Arc<Fixture> {
    let mut fx = Fixture::new().await;
    fx.daemon = SharedCodexAppServer::new_stub(fx.repo.clone());
    let card = fx.repo.card_get(&fx.card).await.unwrap().unwrap();
    fx.backend = PlannerBackend::codex(
        fx.daemon.clone(),
        &fx.card,
        card.track_id.as_str(),
        &fx.runtime,
    )
    .unwrap();
    Arc::new(fx)
}

async fn fail(fx: &Fixture) {
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    crate::db::sqlite::session_fail_if_active_runtime_tx(&mut tx, &fx.runtime)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn state_and_inputs(fx: &Fixture) -> (WorkerSessionState, ConversationInputCheckpoint) {
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.runtime.clone()))
        .await
        .unwrap()
        .unwrap();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    let inputs = carrier.checkpoint().clone();
    assert!(
        current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
            .await
            .is_err(),
        "recording facts cannot restore execution admission"
    );
    assert!(matches!(
        revision_tx(&mut tx, &fx.message).await.unwrap().payload,
        PrivateHistoryPayload::Present { .. }
    ));
    tx.rollback().await.unwrap();
    (row.state, inputs)
}

#[tokio::test]
async fn actual_native_receipt_and_end_record_on_failed_current_carrier_without_reactivation() {
    let fx = real().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/start");
    assert!(call["params"]["input"].to_string().contains(BODY));
    notify_thread(
        &fx.daemon,
        &mut peer,
        "thread",
        "turn/started",
        json!({"id":"failed-original"}),
    )
    .await;
    // The actual request has crossed. Failure commits while its native reply is
    // still in flight, through the same production failure writer as teardown.
    fail(&fx).await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"failed-original"}}}),
    )
    .await;
    let settled = dispatch
        .await
        .unwrap()
        .expect("a returned native fact must survive runtime failure");
    assert!(matches!(
        settled.report.delivery(),
        InputDelivery::Accepted(_)
    ));
    let (state, before) = state_and_inputs(&fx).await;
    assert_eq!(state, WorkerSessionState::Failed);
    assert!(before.active.is_some());
    assert!(before.pending.is_empty());
    notify_thread(
        &fx.daemon,
        &mut peer,
        "thread",
        "turn/completed",
        json!({"id":"failed-original","status":"failed"}),
    )
    .await;
    assert!(
        complete_once(fx.repo.as_ref(), &fx.backend, &fx.card, &fx.runtime)
            .await
            .unwrap()
            .is_some()
    );
    let (state, after) = state_and_inputs(&fx).await;
    assert_eq!(state, WorkerSessionState::Failed);
    assert!(after.active.is_none());
    assert_eq!(after.attempts, before.attempts);
    assert!(fx.dispatch().await.is_err());
}

#[tokio::test]
async fn actual_pre_send_proof_restores_pending_on_failed_current_carrier_without_reactivation() {
    let fx = real().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let gate = fx.daemon.hold_connection_read_for_test().await;
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.checkpoint().await.attempt(&fx.submission).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fail(&fx).await;
    drop(gate);
    let settled = dispatch
        .await
        .unwrap()
        .expect("the actual native preflight fact must settle on its failed carrier");
    assert!(matches!(
        settled.report.delivery(),
        InputDelivery::NotDispatched(_)
    ));
    let (state, inputs) = state_and_inputs(&fx).await;
    assert_eq!(state, WorkerSessionState::Failed);
    assert!(inputs.active.is_none());
    assert_eq!(inputs.pending.len(), 1);
    assert_eq!(
        inputs.pending[0].accepted_input_ref().unwrap(),
        AcceptedInputRef::Human {
            message: fx.message.clone()
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut peer))
            .await
            .is_err()
    );
    assert!(fx.dispatch().await.is_err());
}
