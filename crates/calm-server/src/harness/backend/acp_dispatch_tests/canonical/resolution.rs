//! Authenticated Keep settles only after qualified receipt-specific cleanup.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_acp_unknown_keep_is_idempotent_and_forced_retry_has_no_successor() {
    let fx = Fixture::new("lost").await;
    let selection = TurnModelSelection {
        model: Some("fixture/model-a".into()),
        effort: Some("normal".into()),
    };
    let report = crate::harness::input_dispatch::initial(
        fx.owner.repo.as_ref(),
        &fx.source,
        &fx.owner.card,
        &fx.owner.worker,
        &fx.submission,
        &fx.backend,
        &selection,
        PlannerPermissionMode::Never,
    )
    .await
    .unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    fx.wait_idle().await;
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.state,
        crate::db::sqlite::AcpSubmissionState::Unknown
    ));
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let attempt = carrier
        .checkpoint()
        .attempt(&fx.submission)
        .unwrap()
        .clone();
    tx.commit().await.unwrap();
    let retry = RetryConversationInputRequest {
        carrier_id: fx.owner.worker.clone(),
        submission_id: fx.submission.as_str().into(),
        attempt_id: attempt.attempt.attempt_id.as_str().into(),
        revision: attempt.revision.to_string(),
    };
    assert!(
        crate::conversation_inputs::resolution::retry(
            fx.owner.repo.as_ref(),
            fx.source.clone(),
            fx.owner.card.clone(),
            "forced-retry".into(),
            retry.clone()
        )
        .await
        .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_input_retry_relations")
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let keep = KeepConversationInputRequest::from(&retry);
    let first = crate::conversation_inputs::resolution::keep(
        fx.owner.repo.as_ref(),
        fx.source.clone(),
        fx.owner.card.clone(),
        "keep".into(),
        keep.clone(),
    )
    .await
    .unwrap()
    .0;
    let second = crate::conversation_inputs::resolution::keep(
        fx.owner.repo.as_ref(),
        fx.source.clone(),
        fx.owner.card.clone(),
        "keep".into(),
        keep,
    )
    .await
    .unwrap()
    .0;
    assert_eq!(first, second);
    assert!(first.waiting_for_settlement);
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Settled
    ));
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    let after = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(receipt.input_json, after.input_json);
    assert_eq!(receipt.outcome_json, after.outcome_json);
    assert!(matches!(
        after.state,
        crate::db::sqlite::AcpSubmissionState::Unknown
    ));
    fx.session.shutdown().await.unwrap();
}
