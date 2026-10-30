//! A supplemental claim and its crossing marker are one atomic write.
use super::*;
use calm_types::conversation::{InputInvocation, MessageRevisionRef};
use calm_types::conversation_controls::QueuedMessageSteerResult;
use calm_types::conversation_display::ConversationDelivery;

pub(crate) async fn supplemental(
    repo: &dyn Repo,
    source: crate::auth::HumanInputSource,
    card: &str,
    worker: &str,
    message: MessageRevisionRef,
    expected_turn: String,
    backend: &PlannerBackend,
) -> Result<(
    QueuedMessageSteerResult,
    Vec<calm_types::conversation::CanonicalPendingIdentity>,
)> {
    if !PlannerBackend::metadata(&backend.provider()).steer {
        return Err(CalmError::Conflict(
            "this provider does not support steering".into(),
        ));
    }
    let (card, worker, provider) = (card.to_owned(), worker.to_owned(), backend.provider());
    let mut prepared = write_in_tx_typed(repo, {
        let (card, worker) = (card.clone(), worker.clone());
        move |tx| {
            Box::pin(async move {
                let mut carrier = current_input_carrier_tx(tx, &card, &worker).await?;
                let target = retained_target_tx(tx, &card, &worker, &provider)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Conflict(
                            "there is no confirmed current execution to steer".into(),
                        )
                    })?;
                if target.scope.thread_and_turn().1 != expected_turn
                    || carrier.phase()? != HarnessPhaseTag::TurnRunning
                {
                    return Err(CalmError::Conflict(
                        "the response being steered has changed".into(),
                    ));
                }
                let plan = crate::db::sqlite::conversation::pending_plan_for_message_tx(
                    tx, &carrier, &message,
                )
                .await?;
                let source = InputSource::from(source);
                let (execution, _) = crate::conversation_inputs::authorize_execution_tx(
                    tx, &source, &carrier, &plan,
                )
                .await?;
                let initial =
                    submission_identity_tx(tx, &target.proof.attempt().submission_id).await?;
                if initial.execution != execution {
                    return Err(CalmError::Forbidden(
                        "supplemental execution provenance changed".into(),
                    ));
                }
                let invocation = match &carrier
                    .checkpoint()
                    .attempt(&target.proof.attempt().submission_id)
                    .ok_or_else(|| {
                        CalmError::Conflict("original execution has no invocation".into())
                    })?
                    .invocation
                {
                    InputInvocation::Recorded { configuration } => configuration.clone(),
                    InputInvocation::PriorInvocationNotRecorded => {
                        return Err(CalmError::Conflict(
                            "original invocation is not recorded".into(),
                        ));
                    }
                };
                let claimed = crate::db::sqlite::conversation::claim_supplemental_submission_tx(
                    tx,
                    &mut carrier,
                    &message,
                )
                .await?;
                // No cancellable gap between claim and marker: prepare_tx uses the same SQL transaction.
                prepare_tx(
                    tx,
                    source,
                    card,
                    worker,
                    claimed.submission_id,
                    provider,
                    invocation,
                    Some(target),
                )
                .await
            })
        }
    })
    .await?;
    let target = prepared
        .supplemental_target
        .clone()
        .expect("supplemental preparation binds a target");
    let turn_id = target.scope.thread_and_turn().1;
    let report = backend
        .dispatch_supplemental(
            &target,
            &prepared.proof,
            std::mem::take(&mut prepared.items),
            &crate::harness::native_admission::NativePromptAdmission::new(
                || prepared.source.check_current(),
                || Box::pin(prepared.admit(repo)),
            ),
        )
        .await;
    let committed = settle(repo, &card, &worker, prepared.proof.attempt(), report).await?;
    let result = QueuedMessageSteerResult {
        submission_id: prepared.proof.attempt().submission_id.to_string(),
        turn_id,
        delivery: match committed.report.delivery() {
            InputDelivery::Accepted(_) => ConversationDelivery::Accepted,
            InputDelivery::NotDispatched(_) => ConversationDelivery::NotDispatched,
            InputDelivery::Unknown(_) => ConversationDelivery::Unknown,
        },
    };
    Ok((result, committed.consumed))
}
