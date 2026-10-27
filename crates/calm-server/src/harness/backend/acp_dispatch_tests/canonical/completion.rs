//! Completion retries cannot revoke a successor or erase normally joined cleanup.
use super::super::super::dispatch_fixture::fail_terminal_outcome;
use super::*;
use crate::acp_planner::test_seams::{IssueStage, pause_issue};
use calm_truth::db::sqlite::provider_execution as claims;

#[tokio::test]
async fn canonical_acp_stale_completion_preserves_successor_credential() {
    let fx = Arc::new(Fixture::new("reply").await);
    let pause = pause_issue(&fx.owner.worker, IssueStage::AfterReceipt);
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await.unwrap() }
    });
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let old = claims::load(fx.owner.repo.pool(), &fx.owner.worker)
        .await
        .unwrap()
        .unwrap();
    pause.release.notify_one();
    dispatch.await.unwrap();
    fx.wait_idle().await;
    fx.wait_end().await;
    let receipt = fx.receipt_bytes(&fx.submission).await;
    let successor = fx.claim_successor().await;
    // E2 has minted its real credential and admitted its claim, but has not sent a prompt.
    let pause = pause_issue(&fx.owner.worker, IssueStage::AfterSpawn);
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch_submission(&successor).await }
    });
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let current = claims::load(fx.owner.repo.pool(), &fx.owner.worker)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(old.record().execution_id, current.record().execution_id);
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert!(token.is_some());
    let owner = &fx.session.host().provider_execution;
    owner
        .finish(
            &fx.owner.repo.sqlite_db().unwrap(),
            fx.session.host().execution.engine(),
            &fx.owner.worker,
            &old,
            None,
        )
        .await
        .unwrap();
    let after: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    let retained = claims::load(fx.owner.repo.pool(), &fx.owner.worker)
        .await
        .unwrap();
    let old_receipt = fx.receipt_bytes(&fx.submission).await;
    pause.release.notify_one();
    dispatch.await.unwrap();
    fx.wait_idle().await;
    fx.session.shutdown().await.unwrap();
    assert_eq!(
        after, token,
        "stale E1 completion must not revoke E2's credential"
    );
    assert_eq!(retained, Some(current));
    assert_eq!(old_receipt, receipt, "reuse the durable receipt winner");
}

#[tokio::test]
async fn canonical_acp_cancelled_receipt_retires_and_keep_continues_without_replay() {
    cancelled_receipt(false).await;
}

#[tokio::test]
async fn canonical_acp_cancelled_receipt_projection_failure_preserves_cleanup_join() {
    cancelled_receipt(true).await;
}

async fn cancelled_receipt(projection_failure: bool) {
    let fx = Arc::new(Fixture::new("reply").await);
    let pause = pause_issue(&fx.owner.worker, IssueStage::AfterReceipt);
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let claim = claims::load(fx.owner.repo.pool(), &fx.owner.worker)
        .await
        .unwrap()
        .unwrap();
    if projection_failure {
        fail_terminal_outcome(fx.owner.repo.as_ref()).await;
    }
    dispatch.abort();
    assert!(matches!(dispatch.await, Err(error) if error.is_cancelled()));
    fx.wait_driver_finished().await;
    assert_eq!(
        claims::load(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
            .is_some(),
        projection_failure,
        "only a failed owner projection retains the claim"
    );
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(&claim.record().execution_id)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(desired, if projection_failure { "stop" } else { "retire" });
    let before = fx.receipt_bytes(&fx.submission).await;
    assert_eq!(before.2, "unknown");
    assert_eq!(fx.prompts(), 0);
    // The aborted outer caller lost its private report. Supply the exact
    // receipt correlation through the fact owner; never dispatch a second attempt.
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    let crate::db::sqlite::AcpSubmissionInput::Canonical(input) =
        crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json).unwrap()
    else {
        panic!("canonical receipt");
    };
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    record_input_attempt_fact_tx(
        &mut tx,
        &mut carrier,
        InputAttemptFact {
            attempt: input.proof.attempt().clone(),
            correlation: InputCorrelationFact {
                provider: fx.provider.clone(),
                thread_id: receipt.thread_id,
                native_session_id: Some(receipt.native_session_id),
                native_turn_id: None,
                local_turn_id: Some(receipt.turn_id),
                rpc_request_id: None,
            },
            delivery: InputDeliveryFact::Unknown,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fx.keep_unknown(&fx.submission, "keep-cancelled").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    assert_eq!(fx.prompts(), 0, "Keep never resends the cancelled prompt");
    assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
    if projection_failure {
        sqlx::query("DROP TRIGGER fail_outcome")
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        // A receipt-only retry retires the retained claim without provider I/O.
        fx.session
            .host()
            .provider_execution
            .finish(
                &fx.owner.repo.sqlite_db().unwrap(),
                fx.session.host().execution.engine(),
                &fx.owner.worker,
                &claim,
                None,
            )
            .await
            .unwrap();
    }
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    fx.wait_end().await;
    assert_eq!(
        fx.prompts(),
        1,
        "only fresh Human input reaches the provider"
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_prompt_response_removes_delivery_warning_without_keep() {
    for provider in [AgentProvider::OpenCode, AgentProvider::Grok] {
        let fx = Fixture::with_provider(provider, "reply", false).await;
        fx.dispatch().await.unwrap();
        let frame = fx.wait_end().await;
        let cp: crate::harness::snapshot::CanonicalHarnessSnapshot =
            serde_json::from_value(frame.snapshot).unwrap();
        let cp = cp.canonical_inputs;
        assert!(matches!(
            cp.attempts[0].delivery,
            InputDeliveryFact::Accepted { .. }
        ));
        assert!(cp.receipts.is_empty());
        use crate::db::RepoRead;
        assert!(
            fx.owner
                .repo
                .conversation_read_frame(&fx.owner.card)
                .await
                .unwrap()
                .active_input
                .is_none()
        );
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_acknowledgement_failure_rolls_back_end_and_retries_once() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    sqlx::query("CREATE TRIGGER fail_input_ack BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN json_extract(NEW.handle_state_json,'$.canonical_inputs.attempts[0].delivery.kind')='accepted'
        BEGIN SELECT RAISE(ABORT,'ack failed'); END")
        .execute(fx.owner.repo.pool()).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match crate::harness::input_dispatch::complete_once(
                fx.owner.repo.as_ref(),
                &fx.backend,
                &fx.owner.card,
                &fx.owner.worker,
            )
            .await
            {
                Err(error) => break error,
                Ok(None) => tokio::task::yield_now().await,
                Ok(Some(_)) => panic!("END committed without its input acknowledgement"),
            }
        }
    })
    .await
    .unwrap();
    assert!(error.to_string().contains("ack failed"));
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert!(carrier.checkpoint().active.is_some());
    assert!(matches!(
        carrier.checkpoint().attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    tx.commit().await.unwrap();
    sqlx::query("DROP TRIGGER fail_input_ack")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    fx.wait_end().await;
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}
