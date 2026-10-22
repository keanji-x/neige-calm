//! Protected runtime binding on canonical loop installation.
use super::*;

/// The supplied snapshot selected a contract, not a reusable execution fence.
/// Read the actual current carrier before the loop consumes any commands or
/// native events. No authorization, native readiness or source is fabricated.
pub(in crate::harness::run_loop) async fn load_current(
    params: &PlannerHarnessParams,
) -> Result<CommittedFrame> {
    let card = params.card_id.to_string();
    let track = params.track_id.to_string();
    let runtime = params.worker_session_id.clone();
    let supplied_thread = params.thread_id.clone();
    let provider = params.backend.provider();
    let scope = match params.track_area_cache.area_of(&params.track_id) {
        Some(area) => EventScope::Card {
            card: params.card_id.clone(),
            track: params.track_id.clone(),
            area,
        },
        None => EventScope::System,
    };
    let (committed, events) = write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_carrier_tx(tx, &card, &runtime).await?;
            let snapshot = checked_snapshot(carrier.snapshot().clone()).map_err(|_| {
                CalmError::Conflict("current canonical carrier has an invalid frame".into())
            })?;
            let (binding, current) = input_carrier_binding_tx(tx, &carrier).await?;
            if current.track_id.as_str() != track
                || binding.provider != provider
                || current.thread_id != snapshot.header.last_thread_id
                || supplied_thread
                    .as_ref()
                    .is_some_and(|thread| current.thread_id.as_ref() != Some(thread))
            {
                return Err(CalmError::Conflict(
                    "canonical runtime parameters differ from its current binding".into(),
                ));
            }
            if snapshot.header.phase == HarnessPhaseTag::TurnCompleted
                && snapshot
                    .header
                    .last_turn_id
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err(CalmError::Conflict(
                    "completed canonical frame has no exact original turn".into(),
                ));
            }
            if snapshot.header.phase == HarnessPhaseTag::Compacting
                || snapshot
                    .header
                    .compact_outcome
                    .as_ref()
                    .is_some_and(|outcome| {
                        outcome.status == crate::harness::compact_outcome::CompactStatus::Started
                    })
            {
                let previous = snapshot.header.clone();
                let mut snapshot = snapshot;
                super::compact_command::canonical::wedge(&mut snapshot);
                let events = super::compact_command::canonical::planner::append_transition_tx(
                    tx,
                    &TrackId(track),
                    &scope,
                    &previous,
                    &snapshot.header,
                    false,
                )
                .await?;
                let committed = written(
                    persist_runtime_frame_tx(
                        tx,
                        &card,
                        &runtime,
                        &RuntimeExecutionFence::from_carrier(&carrier),
                        header_value(&snapshot)?,
                    )
                    .await?,
                )?;
                return Ok((committed, events));
            }
            Ok((
                CommittedFrame {
                    snapshot,
                    fence: RuntimeExecutionFence::from_carrier(&carrier),
                },
                vec![],
            ))
        })
    })
    .await?;
    for event in events {
        params.events.emit_envelope(event);
    }
    Ok(committed)
}
