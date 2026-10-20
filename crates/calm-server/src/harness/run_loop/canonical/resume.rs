//! Resume a persisted canonical claim that has not crossed the native boundary.
use super::*;

/// A claim without any crossing can resume only from a newly supplied live
/// source for its exact frozen human revision. Crossing/Unknown/Accepted never
/// flow through this path, even when no native turn is currently visible.
/// Resume the immutable plan as composed, including pre-feature plans without
/// a context-usage segment. Never add or resample context during replay.
pub(super) async fn resume_unattempted(inner: &Arc<Inner>, current: &CommittedFrame) -> Result<()> {
    let checkpoint = &current.snapshot.canonical_inputs;
    let Some(active) = &checkpoint.active else {
        return Ok(());
    };
    let [submission] = active.submission_ids.as_slice() else {
        return Ok(());
    };
    if current.snapshot.header.phase != HarnessPhaseTag::IssuingTurn
        || checkpoint.attempt(submission).is_some()
    {
        return Ok(());
    }
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let expected = current.fence.clone();
    let submission = submission.clone();
    let message = write_in_tx_typed(inner.repo.as_ref(), {
        let (card, runtime, expected, submission) = (
            card.clone(),
            runtime.clone(),
            expected.clone(),
            submission.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                if RuntimeExecutionFence::from_carrier(&carrier) != expected {
                    return Err(CalmError::Conflict("unattempted execution changed".into()));
                }
                let stored =
                    crate::db::sqlite::conversation::submission_plan_tx(tx, &submission).await?;
                let refs = stored
                    .plan
                    .input_refs()
                    .map_err(|error| CalmError::Conflict(error.into()))?;
                Ok(match refs.as_slice() {
                    [input] => Some(input.clone()),
                    _ => None,
                })
            })
        }
    })
    .await?;
    let Some(message) = message else {
        return Ok(());
    };
    let Some(source) = inner
        .inputs
        .canonical()
        .expect("canonical")
        .source(&message)
        .await
    else {
        publish_issuance_error(inner, &CalmError::Unauthorized).await;
        return Ok(());
    };
    let read_admission = write_in_tx_typed(inner.repo.as_ref(), {
        let repo = inner.repo.clone();
        let (submission, source) = (submission.clone(), source.clone());
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                if RuntimeExecutionFence::from_carrier(&carrier) != expected {
                    return Err(CalmError::Conflict("unattempted execution changed".into()));
                }
                crate::conversation_inputs::SelectionReadAdmission::stored_tx(
                    tx, repo, source, &carrier, submission,
                )
                .await
            })
        }
    })
    .await?;
    let resolved = async {
        Ok::<_, IssuanceRefusal>((
            inner.backend.resolve_selection(&read_admission).await?,
            turn_permission::resolve(inner).await?,
        ))
    }
    .await;
    match resolved {
        Ok((selection, permission)) => {
            dispatch_initial(
                inner,
                &source,
                &submission,
                &active.logical_turn_id,
                &selection,
                permission,
            )
            .await
        }
        Err(refusal) => {
            apply_refusal(inner, &refusal).await;
            if let Err(error) = read_admission.revalidate().await {
                publish_issuance_error(inner, &error).await;
            }
            Ok(())
        }
    }
}
