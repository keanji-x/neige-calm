//! Already prepared legacy interruptions must keep the exact original target.
//! Only the old operation receipt is fixture data; runtime phases/turns advance
//! through the historical production loop and provider notifications.
use super::historical_control_admission_tests::HistoricalFixture;
use super::*;
use crate::operation::{
    OperationKey, OperationOutcome, OperationRepo, SqlxOperationRepo, TxOutput,
};

async fn old_interrupt(fx: &HistoricalFixture, output: Option<TxOutput>) -> String {
    let store = SqlxOperationRepo::new(fx.fixture.repo.pool().clone());
    let payload = json!({"runtime_id":fx.worker,"reason":"user_stop"});
    let id = store
        .insert_operation(
            "planner-harness-interrupt",
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: None,
                payload_hash: super::super::idempotency_key::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    if let Some(output) = output {
        // Released old TxOutput shape, built from the real original runtime.
        // No launch/claim/FSM fact is invented by this fixture write.
        sqlx::query("UPDATE operations SET phase='tx_committed',tx_output_json=?2,target_type=?3,target_id=?4,target_json=?5 WHERE id=?1")
            .bind(&id)
            .bind(serde_json::to_string(&output).unwrap())
            .bind(&output.target_type).bind(&output.target_id)
            .bind(json!({"type":output.target_type,"id":output.target_id}).to_string())
            .execute(fx.fixture.repo.pool())
            .await
            .unwrap();
    }
    id
}

async fn original_output(fx: &HistoricalFixture) -> TxOutput {
    let runtime = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let runtime = fx
                .fixture
                .repo
                .session_projection_by_id(&fx.worker)
                .await
                .unwrap()
                .unwrap();
            if runtime.active_turn_id.as_deref() == Some("fake-turn-0001") {
                break runtime;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("original running projection is actually committed");
    assert_eq!(runtime.active_turn_id.as_deref(), Some("fake-turn-0001"));
    assert!(runtime.thread_id.is_some());
    let mut output = TxOutput::new(
        "runtime",
        Some(fx.worker.clone()),
        serde_json::to_value(runtime).unwrap(),
    );
    output.data = json!({"runtime_id":fx.worker,"reason":"user_stop"});
    output
}

async fn drive(fx: &HistoricalFixture, id: &str) -> OperationOutcome {
    fx.state.operation_runtime.drive().await.unwrap();
    fx.state
        .operation_runtime
        .wait(&id.to_owned())
        .await
        .unwrap()
        .outcome
}

#[tokio::test]
async fn prepared_legacy_interrupt_restores_its_original_running_target() {
    let fx = HistoricalFixture::new(true).await;
    let original = original_output(&fx).await;
    let expected = (
        original.result["thread_id"].as_str().unwrap().to_owned(),
        "fake-turn-0001".to_owned(),
    );
    let id = old_interrupt(&fx, Some(original)).await;
    let outcome = drive(&fx, &id).await;
    let interrupted = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert!(matches!(outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(interrupted, vec![expected]);
}

#[tokio::test]
async fn prepared_legacy_interrupt_cannot_retarget_a_later_turn() {
    let fx = HistoricalFixture::new(true).await;
    let original = original_output(&fx).await;
    let thread = original.result["thread_id"].as_str().unwrap().to_owned();
    let id = old_interrupt(&fx, Some(original)).await;
    fx.state.shared_codex_appserver.emit_notification_for_test(
        crate::codex_appserver::Notification::TurnCompleted {
            thread_id: thread,
            turn: json!({"id":"fake-turn-0001","status":"completed"}),
        },
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.harness.runtime_header().await.unwrap().phase != HarnessPhaseTag::TurnCompleted {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("original turn completes through the provider reducer");
    post(
        &fx.app,
        &fx.cookie,
        &format!("/api/cards/{}/planner/input", fx.card),
        "later-turn",
        json!({"text":"A genuinely later historical turn"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = fx.harness.snapshot().await.unwrap();
            if snapshot.phase == HarnessPhaseTag::TurnRunning
                && snapshot.last_turn_id.as_deref() == Some("fake-turn-0002")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ordinary input starts the different later native turn");
    let _outcome = drive(&fx, &id).await;
    let interrupted = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    let phase = fx.harness.runtime_header().await.unwrap().phase;
    fx.shutdown().await;
    assert!(
        interrupted.is_empty(),
        "the old receipt must not stop the later turn: {interrupted:?}"
    );
    assert_eq!(phase, HarnessPhaseTag::TurnRunning);
}

#[tokio::test]
async fn prepared_legacy_interrupt_without_an_original_target_refuses_current_turn() {
    let fx = HistoricalFixture::new(true).await;
    let mut original = original_output(&fx).await;
    original.result["active_turn_id"] = Value::Null;
    let id = old_interrupt(&fx, Some(original)).await;
    let outcome = drive(&fx, &id).await;
    let interrupted = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert!(
        interrupted.is_empty(),
        "missing original target cannot borrow a current turn"
    );
    assert!(!matches!(outcome, OperationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn pending_legacy_interrupt_cannot_discover_a_current_target_on_recovery() {
    let fx = HistoricalFixture::new(true).await;
    let id = old_interrupt(&fx, None).await;
    let outcome = drive(&fx, &id).await;
    let interrupted = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert!(
        interrupted.is_empty(),
        "unprepared old request has no original target"
    );
    assert!(!matches!(outcome, OperationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn new_unbound_legacy_interrupt_submissions_are_refused() {
    let fx = HistoricalFixture::new(true).await;
    let payload = json!({"runtime_id":fx.worker,"reason":"user_stop"});
    let submitted = fx
        .state
        .operation_runtime
        .submit(
            "planner-harness-interrupt",
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: None,
                payload_hash: super::super::idempotency_key::stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await;
    // Finish the accepted old path before the decisive red assertion.
    let accepted = submitted.is_ok();
    if let Ok(id) = submitted {
        let _ = fx.state.operation_runtime.wait(&id).await.unwrap();
    }
    fx.shutdown().await;
    assert!(
        !accepted,
        "new runtime-only submissions lack an original target and live public source"
    );
}

#[tokio::test]
async fn old_interrupt_cannot_override_its_original_request_target() {
    let fx = HistoricalFixture::new(true).await;
    let original = original_output(&fx).await;
    let id = old_interrupt(&fx, Some(original)).await;
    let original_request = json!({"runtime_id":"different-original-runtime","reason":"user_stop"});
    sqlx::query("UPDATE operations SET payload_json=?2,payload_hash=?3 WHERE id=?1")
        .bind(&id)
        .bind(original_request.to_string())
        .bind(super::super::idempotency_key::stable_payload_hash(&original_request).unwrap())
        .execute(fx.fixture.repo.pool())
        .await
        .unwrap();
    let outcome = drive(&fx, &id).await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert!(
        native.is_empty(),
        "a prepared result cannot replace the original request's target"
    );
    assert!(!matches!(outcome, OperationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn old_interrupt_cannot_override_its_operation_target_metadata() {
    let fx = HistoricalFixture::new(true).await;
    let original = original_output(&fx).await;
    let id = old_interrupt(&fx, Some(original)).await;
    sqlx::query("UPDATE operations SET target_id='different-original-runtime' WHERE id=?1")
        .bind(&id)
        .execute(fx.fixture.repo.pool())
        .await
        .unwrap();
    let outcome = drive(&fx, &id).await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert!(
        native.is_empty(),
        "prepared data cannot replace the operation's stored target"
    );
    assert!(!matches!(outcome, OperationOutcome::Succeeded { .. }));
}
