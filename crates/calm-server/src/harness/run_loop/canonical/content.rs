//! Native display and telemetry observations; input and execution facts stay with their owners.
use super::*;

pub(in crate::harness::run_loop) async fn on_notification(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    source_thread: Option<&str>,
    kind: PlannerEventKind,
) -> Result<()> {
    match kind {
        PlannerEventKind::Item {
            phase,
            params,
            questions,
        } => {
            content_observer::on_item(inner, live, phase, params, questions).await?;
        }
        PlannerEventKind::ReplyDelta {
            turn_id,
            item_id,
            delta,
        } => live.delta(&turn_id, &item_id, &delta),
        PlannerEventKind::TokenUsage { params } => {
            observe_metadata(inner, Metadata::Usage(params)).await?;
        }
        PlannerEventKind::ContextReplaced { turn_id } => {
            if !super::super::compact_command::canonical::boundary(inner, &turn_id).await? {
                observe_metadata(inner, Metadata::ContextBoundary(turn_id)).await?;
            }
            super::super::compact_command::canonical::observe(inner).await?;
        }
        PlannerEventKind::TurnStarted { turn_id } => {
            if super::super::compact_command::canonical::observe(inner).await? {
                return Ok(());
            }
            let current = read_current(inner).await?;
            // This only opens the installed ephemeral reply writer. The actual
            // report owner has already selected the current native execution;
            // raw start events cannot change phase, delivery or active claims.
            if current.snapshot.header.phase == HarnessPhaseTag::TurnRunning
                && current.snapshot.header.last_turn_id.as_deref() == Some(turn_id.as_str())
                && let Some(thread) = source_thread.filter(|thread| !thread.is_empty())
                && current.snapshot.header.last_thread_id.as_deref() == Some(thread)
            {
                live.turn_started_native(thread, &turn_id);
            }
            consume_end(inner).await?;
        }
        PlannerEventKind::TurnCompleted { turn } => {
            if super::super::compact_command::canonical::observe(inner).await? {
                return Ok(());
            }
            if let Some(turn_id) = turn.get("id").and_then(Value::as_str) {
                live_reply::settle(inner, live, turn_id, &turn).await;
            }
            consume_end(inner).await?;
        }
        PlannerEventKind::TurnAborted { .. } | PlannerEventKind::ThreadIdle => {
            consume_end(inner).await?;
        }
        PlannerEventKind::ThreadSystemError => {
            super::control::failed(inner, HARNESS_SYSTEM_ERROR_REASON).await?;
        }
        PlannerEventKind::ThreadStarted
        | PlannerEventKind::PlanUpdated { .. }
        | PlannerEventKind::Approval { .. }
        | PlannerEventKind::Ignored => {}
    }
    Ok(())
}

async fn consume_end(inner: &Arc<Inner>) -> Result<()> {
    if let Some(done) = crate::harness::input_dispatch::complete_once(
        inner.repo.as_ref(),
        &inner.backend,
        inner.card_id.as_str(),
        &inner.worker_session_id,
    )
    .await?
    {
        publish_end(inner, done).await?;
    }
    Ok(())
}

enum Metadata {
    Usage(Value),
    ContextBoundary(String),
}

async fn observe_metadata(inner: &Arc<Inner>, observation: Metadata) -> Result<()> {
    let card = inner.card_id.to_string();
    let worker = inner.worker_session_id.clone();
    let next = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            let mut next: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone())?;
            let header = &mut next.header;
            // A preceding turn's last_turn_id can remain on an Issuing frame.
            // Neither its late usage nor an ambiguous new turn may update it.
            if !matches!(
                header.phase,
                HarnessPhaseTag::TurnRunning | HarnessPhaseTag::TurnCompleted
            ) {
                return Ok(None);
            }
            match observation {
                Metadata::Usage(params) => {
                    let reading_turn = item_turn_id(&params);
                    if reading_turn.is_none() || reading_turn != header.last_turn_id.as_deref() {
                        return Ok(None);
                    }
                    let Some(reading) = TokenUsage::from_params(&params, crate::model::now_ms())
                    else {
                        return Ok(None);
                    };
                    header.token_usage = Some(reading.sticky_merge(header.token_usage.as_ref()));
                    // Preserve the existing automatic-context-boundary rule:
                    // billing may continue without claiming retained occupancy.
                    if !header.context_usage_invalidated
                        || reading_turn != header.context_usage_invalidated_turn.as_deref()
                    {
                        header.context_usage_invalidated = false;
                        header.context_usage_invalidated_turn = None;
                    }
                }
                Metadata::ContextBoundary(turn) => {
                    if header.last_turn_id.as_deref() != Some(turn.as_str()) {
                        return Ok(None);
                    }
                    invalidate_context_usage(header);
                }
            }
            let persisted = persist_runtime_frame_tx(
                tx,
                &card,
                &worker,
                &RuntimeExecutionFence::from_carrier(&carrier),
                header_value(&next)?,
            )
            .await?;
            Ok(Some(written(persisted)?))
        })
    })
    .await?;
    if let Some(next) = next {
        publish(inner, next).await;
    }
    Ok(())
}

pub(in crate::harness::run_loop) async fn persist_native_item(
    inner: &Arc<Inner>,
    thread: &str,
    turn: Option<&str>,
    method: &str,
    params: &Value,
) -> Result<Option<crate::db::sqlite::conversation::StoredNativePresentation>> {
    let card = inner.card_id.to_string();
    let worker = inner.worker_session_id.clone();
    let thread = thread.to_owned();
    let turn = turn.map(str::to_owned);
    let method = method.to_owned();
    let params = params.clone();
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            Ok(crate::db::sqlite::conversation::observe_native_item_tx(
                tx,
                &carrier,
                &thread,
                turn.as_deref(),
                &method,
                &params,
            )
            .await?)
        })
    })
    .await
}

/// The retained-context boundary is keyed to the user's watermark, including
/// manual maintenance that has its own local execution identity.
pub(in crate::harness::run_loop) fn invalidate_context_usage(
    header: &mut calm_types::harness::snapshot::HarnessRuntimeHeader,
) {
    header.context_usage_invalidated = true;
    header.context_usage_invalidated_turn = header.last_turn_id.clone();
}
