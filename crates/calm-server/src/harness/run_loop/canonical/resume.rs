//! Resume a persisted canonical claim that has not crossed the native boundary.
use super::*;

/// A claim without any crossing can resume only from a newly supplied live
/// source for its exact frozen human revision. Crossing/Unknown/Accepted never
/// flow through this path, even when no native turn is currently visible.
/// Resume the immutable plan as composed, including pre-feature plans without
/// a context-usage segment. Never add or resample context during replay.
pub(super) async fn resume_unattempted(
    inner: &Arc<Inner>,
    current: &CommittedFrame,
    issuance: tokio::sync::MutexGuard<'_, ()>,
) -> Result<Progress> {
    let checkpoint = &current.snapshot.canonical_inputs;
    let waiting = match current.snapshot.header.phase {
        HarnessPhaseTag::TurnRunning => Progress::Waiting,
        _ => Progress::Deferred,
    };
    let Some(active) = &checkpoint.active else {
        return Ok(Progress::Deferred);
    };
    let [submission] = active.submission_ids.as_slice() else {
        return Ok(waiting);
    };
    if current.snapshot.header.phase != HarnessPhaseTag::IssuingTurn
        || checkpoint.attempt(submission).is_some()
    {
        return Ok(waiting);
    }
    // Re-enter in Track → issuance order. The original expected frame below
    // remains the writer fence if another command changed the head while waiting.
    drop(issuance);
    let super::super::issuance::TrackIssuance::Acquired(track) =
        super::super::issuance::lock_track_for_admission(inner).await?
    else {
        return Ok(Progress::Deferred);
    };
    let _issuance = inner.issuance.lock().await;
    if inner.shutting_down.load(Ordering::SeqCst) || inner.parked.is_parked() {
        return Ok(Progress::Deferred);
    }
    let track_fence = crate::provider_execution::TrackFenceHandoff::new(track);
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("c5d-resume-admission-ready", &inner.worker_session_id).await;
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
                if let Some(retry_intent_id) =
                    crate::db::sqlite::conversation::submission_retry_intent_tx(tx, &submission)
                        .await?
                {
                    crate::db::sqlite::conversation::retry_intent_tx(
                        tx,
                        &carrier,
                        &retry_intent_id,
                    )
                    .await?;
                    return Ok(Some(CanonicalPendingIdentity::ExplicitSuccessor {
                        retry_intent_id,
                    }));
                }
                let stored =
                    crate::db::sqlite::conversation::submission_plan_tx(tx, &submission).await?;
                let refs = stored
                    .plan
                    .input_refs()
                    .map_err(|error| CalmError::Conflict(error.into()))?;
                Ok(match refs.as_slice() {
                    [input] => Some(CanonicalPendingIdentity::Accepted {
                        input: input.clone(),
                    }),
                    _ => None,
                })
            })
        }
    })
    .await?;
    let Some(message) = message else {
        return Ok(Progress::Deferred);
    };
    let Some(source) = inner
        .inputs
        .canonical()
        .expect("canonical")
        .execution_source(&message)?
    else {
        return Ok(Progress::Deferred);
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
    if let CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id } = &message {
        let card = inner.card_id.to_string();
        let runtime = inner.worker_session_id.clone();
        let id = retry_intent_id.clone();
        let invocation = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                Ok(
                    crate::db::sqlite::conversation::retry_intent_tx(tx, &carrier, &id)
                        .await?
                        .invocation,
                )
            })
        })
        .await?;
        read_admission.revalidate().await?;
        return dispatch_initial(
            inner,
            &source,
            &submission,
            &active.logical_turn_id,
            &TurnModelSelection {
                model: invocation.model,
                effort: invocation.effort,
            },
            invocation.permission,
            &track_fence,
        )
        .await
        .map(|()| Progress::Deferred);
    }
    let resolved = async {
        Ok::<_, IssuanceRefusal>((
            inner.backend.resolve_selection(&read_admission).await?,
            turn_permission::resolve(inner).await?,
        ))
    }
    .await;
    match resolved {
        Ok((selection, permission)) => dispatch_initial(
            inner,
            &source,
            &submission,
            &active.logical_turn_id,
            &selection,
            permission,
            &track_fence,
        )
        .await
        .map(|()| Progress::Deferred),
        Err(refusal) => {
            apply_refusal(inner, &refusal).await;
            if let Err(error) = read_admission.revalidate().await {
                publish_issuance_error(inner, &error).await;
            }
            Ok(Progress::Deferred)
        }
    }
}
