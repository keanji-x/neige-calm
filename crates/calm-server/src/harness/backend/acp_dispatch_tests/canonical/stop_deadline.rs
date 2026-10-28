//! A Stop deadline wedges the frame; the original's own late END restores it once.
#![cfg(test)]
use super::*;
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use calm_types::harness::HarnessPhaseTag;
use calm_types::worker::WorkerSessionState;

async fn observed(fx: &Fixture) -> (CanonicalHarnessSnapshot, WorkerSessionState) {
    use calm_truth::session_repo::SessionRepo as _;
    let row = fx
        .owner
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.owner.worker.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    (
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap(),
        row.state,
    )
}

async fn wait_frame(
    fx: &Fixture,
    reached: impl Fn(&CanonicalHarnessSnapshot) -> bool,
) -> (CanonicalHarnessSnapshot, WorkerSessionState) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = observed(fx).await;
            if reached(&current.0) {
                return current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the canonical frame reaches the expected state")
}

async fn wait_wedged(fx: &Fixture, original: &CanonicalHarnessSnapshot) {
    let (wedged, row) = wait_frame(fx, |frame| frame.header.phase == HarnessPhaseTag::Wedged).await;
    assert_eq!(
        wedged.header.wedged_reason.as_deref(),
        Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
    );
    assert!(wedged.header.interruption_intent.is_some());
    assert_eq!(
        wedged.canonical_inputs.active,
        original.canonical_inputs.active
    );
    assert_eq!(row, WorkerSessionState::Failed);
}

/// Saved while the frame is wedged, as the incident's later inputs were.
async fn accept_pending(
    fx: &Fixture,
    harness: &crate::harness::PlannerHarness,
) -> MessageRevisionRef {
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_content_carrier_tx(&mut tx, &fx.owner.card)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "pending after stop deadline".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    harness
        .observe_canonical_source(message.clone(), fx.source.clone())
        .await
        .unwrap();
    message
}

async fn restored_once(
    fx: &Fixture,
    harness: &crate::harness::PlannerHarness,
    original: &CanonicalHarnessSnapshot,
    message: MessageRevisionRef,
) {
    let (done, row) = wait_frame(fx, |frame| {
        frame.header.phase == HarnessPhaseTag::TurnCompleted
    })
    .await;
    assert!(done.header.wedged_reason.is_none(), "the wedge is cleared");
    assert!(done.header.interruption_intent.is_none());
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts.len(),
        original.canonical_inputs.attempts.len()
    );
    assert_eq!(
        done.canonical_inputs.pending[0]
            .accepted_input_ref()
            .unwrap(),
        AcceptedInputRef::Human { message }
    );
    assert_eq!(row, WorkerSessionState::Idle);
    tokio::time::timeout(Duration::from_secs(20), async {
        while harness.issuance_block().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the restored END lifts the issuance block");
    let receipt = fx.receipt_bytes(&fx.submission).await;
    assert_eq!(receipt.2, "completed");
    assert_eq!(fx.prompts(), 1);
    harness.resume_issuance_for_test();
    let (next, _) = wait_frame(fx, |frame| {
        frame.canonical_inputs.pending.is_empty()
            && frame.canonical_inputs.attempts.len() == original.canonical_inputs.attempts.len() + 1
            && matches!(
                frame.header.phase,
                HarnessPhaseTag::TurnRunning | HarnessPhaseTag::TurnCompleted
            )
    })
    .await;
    assert!(next.header.wedged_reason.is_none());
    tokio::time::timeout(Duration::from_secs(20), async {
        while fx.prompts() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the pending input reaches the native peer");
    assert_eq!(
        fx.prompts(),
        2,
        "the pending input is dispatched exactly once"
    );
    assert_eq!(
        fx.receipt_bytes(&fx.submission).await,
        receipt,
        "the original receipt is never rewritten or resent"
    );
}

async fn start_loop(fx: &Fixture) -> (crate::harness::PlannerHarness, CanonicalHarnessSnapshot) {
    let harness =
        super::control::started(fx, super::compact::registered_recovery_harness(fx).await).await;
    let (original, _) = observed(fx).await;
    harness.pause_issuance_for_dev();
    (harness, original)
}

#[tokio::test]
async fn canonical_acp_stop_deadline_late_end_restores_and_dispatches_pending_once() {
    let fx = Fixture::new("hold").await;
    let (harness, original) = start_loop(&fx).await;
    let settlement = crate::acp_planner::test_seams::pause_settlement(&fx.owner.worker);
    let stop = tokio::spawn({
        let harness = harness.clone();
        let source = fx.source.clone();
        async move { harness.stop_canonical(source).await }
    });
    tokio::time::timeout(Duration::from_secs(20), settlement.entered.notified())
        .await
        .expect("the cancelled original reaches receipt settlement");
    stop.await.unwrap().unwrap();
    assert_eq!(super::control::cancel_count(&fx), 1);
    harness.expire_interrupt_deadline_for_test().await;
    wait_wedged(&fx, &original).await;
    let message = accept_pending(&fx, &harness).await;
    settlement.release.notify_one();
    restored_once(&fx, &harness, &original, message).await;
    assert_eq!(super::control::cancel_count(&fx), 1);
    let _ = harness.shutdown().await;
}

#[tokio::test]
async fn canonical_acp_stop_deadline_unwritten_cancel_then_end_turn_restores_once() {
    let fx = Fixture::new("settlement").await;
    let (harness, original) = start_loop(&fx).await;
    let unsent = crate::acp_planner::test_seams::pause_before_interrupt_send(&fx.owner.worker);
    let stop = tokio::spawn({
        let harness = harness.clone();
        let source = fx.source.clone();
        async move { harness.stop_canonical(source).await }
    });
    tokio::time::timeout(Duration::from_secs(20), unsent.entered.notified())
        .await
        .expect("Stop is accepted before its native cancel is written");
    let (accepted, _) = observed(&fx).await;
    assert_eq!(accepted.header.phase, HarnessPhaseTag::IssuingInterrupt);
    assert!(accepted.header.interruption_intent.is_some());
    harness.expire_interrupt_deadline_for_test().await;
    wait_wedged(&fx, &original).await;
    let message = accept_pending(&fx, &harness).await;
    // The provider keeps running and ends its original turn normally.
    std::fs::write(fx.dir.path().join("release-settlement"), "").unwrap();
    restored_once(&fx, &harness, &original, message).await;
    assert_eq!(super::control::cancel_count(&fx), 0);
    drop(unsent);
    assert!(
        stop.await.unwrap().is_err(),
        "the resolved Stop cannot write a cancel later"
    );
    assert_eq!(super::control::cancel_count(&fx), 0);
    let _ = harness.shutdown().await;
}
