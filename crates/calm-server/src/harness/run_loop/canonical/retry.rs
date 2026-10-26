//! Explicit intents use frozen text/invocation in the ordinary claim/driver.
use super::*;
use calm_types::conversation::RetryIntentId;

pub(super) async fn dispatch_pending(
    inner: &Arc<Inner>,
    current: &CommittedFrame,
    source: InputSource,
    id: RetryIntentId,
    track_fence: &crate::provider_execution::TrackFenceHandoff,
) -> Result<()> {
    let card = inner.card_id.to_string();
    let runtime = inner.worker_session_id.clone();
    let expected = current.fence.clone();
    let (intent, read) = write_in_tx_typed(inner.repo.as_ref(), {
        let (card, runtime, expected, source, id, repo) = (
            card.clone(),
            runtime.clone(),
            expected.clone(),
            source.clone(),
            id.clone(),
            inner.repo.clone(),
        );
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                require_head(
                    &carrier,
                    &expected,
                    &CanonicalPendingIdentity::ExplicitSuccessor {
                        retry_intent_id: id.clone(),
                    },
                )?;
                let intent =
                    crate::db::sqlite::conversation::retry_intent_tx(tx, &carrier, &id).await?;
                let read = crate::conversation_inputs::SelectionReadAdmission::retry_pending_tx(
                    tx, repo, source, &carrier, id,
                )
                .await?;
                Ok((intent, read))
            })
        }
    })
    .await?;
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || read.check_current(),
        || Box::pin(read.revalidate()),
    );
    match inner
        .backend
        .settled_activity(
            &card,
            current
                .snapshot
                .header
                .last_thread_id
                .as_deref()
                .ok_or_else(|| CalmError::Conflict("retry has no exact thread".into()))?,
            &admission,
        )
        .await?
    {
        crate::harness::backend::SettledActivity::Held => return Ok(()),
        crate::harness::backend::SettledActivity::Settled => {}
    }
    read.revalidate().await?;
    let (claimed, next) = write_in_tx_typed(inner.repo.as_ref(), {
        let source = source.clone();
        move |tx| {
            Box::pin(async move {
                let mut carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
                require_head(
                    &carrier,
                    &expected,
                    &CanonicalPendingIdentity::ExplicitSuccessor {
                        retry_intent_id: id.clone(),
                    },
                )?;
                let claimed = crate::conversation_inputs::claim_initial_tx(
                    tx,
                    source,
                    &mut carrier,
                    crate::db::sqlite::conversation::InitialClaim::ExplicitSuccessor {
                        retry_intent_id: id,
                    },
                )
                .await?;
                Ok((claimed, frame(&carrier)?))
            })
        }
    })
    .await?;
    publish(inner, next).await;
    dispatch_initial(
        inner,
        &source,
        &claimed.submission_id,
        &claimed.logical_turn_id,
        &TurnModelSelection {
            model: intent.invocation.model,
            effort: intent.invocation.effort,
        },
        intent.invocation.permission,
        track_fence,
    )
    .await
}
