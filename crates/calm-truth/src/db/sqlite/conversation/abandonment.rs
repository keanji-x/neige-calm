//! Explicit Human abandonment preserves unresolved facts on a retired carrier.
use super::*;
use crate::planner_binding::PlannerBinding;
use calm_types::conversation::{
    AbandonConversationRequest, AbandonConversationResult, InputDeliveryFact,
};
use calm_types::harness::HarnessPhaseTag;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

/// The caller supplies a provider-qualified recovery mode; it still owns quiescence.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AbandonmentMode {
    UnknownInput,
    UnconfirmedSession,
}

/// Eligibility is separate from ordinary idle replacement. It grants no execution proof.
pub async fn abandonment_request_tx(
    _tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
    mode: AbandonmentMode,
) -> Result<Option<AbandonConversationRequest>> {
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
    if frame
        .canonical_inputs
        .pending
        .iter()
        .any(|entry| !matches!(entry, calm_types::conversation::PendingInput::Human { .. }))
        || !match mode {
            AbandonmentMode::UnknownInput => matches!(
                frame.header.phase,
                HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingTurn
            ),
            AbandonmentMode::UnconfirmedSession => matches!(
                frame.header.phase,
                HarnessPhaseTag::Idle
                    | HarnessPhaseTag::TurnRunning
                    | HarnessPhaseTag::IssuingTurn
                    | HarnessPhaseTag::Resumed
                    | HarnessPhaseTag::TurnCompleted
            ),
        }
        || frame.header.wedged_reason.is_some()
        || carrier
            .snapshot()
            .get("interruption_intent")
            .is_some_and(|v| !v.is_null())
        || carrier
            .snapshot()
            .get("pending_rewind")
            .is_some_and(|v| !v.is_null())
    {
        return Ok(None);
    }
    let fact = match mode {
        AbandonmentMode::UnknownInput => {
            let Some(active) = &frame.canonical_inputs.active else {
                return Ok(None);
            };
            if active.submission_ids.len() != 1
                || frame.canonical_inputs.referenced_submissions() != active.submission_ids
            {
                return Ok(None);
            }
            frame.canonical_inputs.attempt(&active.submission_ids[0])
        }
        AbandonmentMode::UnconfirmedSession => frame.canonical_inputs.attempts.last(),
    };
    let Some(fact) = fact else { return Ok(None) };
    match (&fact.delivery, mode) {
        (InputDeliveryFact::Unknown, _)
        | (
            InputDeliveryFact::CrossingUnknown | InputDeliveryFact::NotDispatched { .. },
            AbandonmentMode::UnconfirmedSession,
        ) => {}
        _ => return Ok(None),
    }
    // Multiple active or supplemental submissions retain their own resolution contract.
    let referenced = frame.canonical_inputs.referenced_submissions();
    if !referenced.is_empty() && referenced != vec![fact.attempt.submission_id.clone()] {
        return Ok(None);
    }
    Ok(Some(AbandonConversationRequest {
        carrier_id: carrier.worker_session_id().into(),
        submission_id: fact.attempt.submission_id.as_str().into(),
        attempt_id: fact.attempt.attempt_id.as_str().into(),
        revision: fact.revision.to_string(),
    }))
}

pub async fn replay_abandonment_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    principal: &str,
    key: &str,
    request: &AbandonConversationRequest,
) -> Result<Option<AbandonConversationResult>> {
    let row: Option<(String,String,String)> = sqlx::query_as(concat!("SELECT request_json,old_worker,new_worker FROM conversation_abandonments WHERE card_id=?1 AND ","principal=?2 AND request_key=?3"))
        .bind(card).bind(principal).bind(key).fetch_optional(&mut **tx).await?;
    row.map(|(json, old, new)| {
        if serde_json::from_str::<AbandonConversationRequest>(&json)? != *request {
            return Err(CalmError::Conflict("abandonment key names another request"));
        }
        Ok(AbandonConversationResult {
            card_id: card.into(),
            old_carrier_id: old,
            carrier_id: new,
        })
    })
    .transpose()
}

pub async fn abandon_unresolved_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    expected: ExpectedCarrier<'_>,
    mode: AbandonmentMode,
    request: &AbandonConversationRequest,
    principal: &str,
    key: &str,
) -> Result<CarrierReplacement> {
    let carrier =
        current_input_fact_carrier_tx(tx, &expected.card_id, &expected.worker_session_id).await?;
    let shape = super::super::card_execution_shape_tx(tx, &expected.card_id).await?;
    let binding = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
        .ok_or(CalmError::Conflict("recovery binding unsupported"))?;
    let old = super::super::session_get_tx(
        tx,
        &crate::worker::WorkerSessionId(expected.worker_session_id.clone()),
    )
    .await?
    .ok_or(CalmError::Conflict("recovery session missing"))?;
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
    if expected.worker_session_id != request.carrier_id
        || carrier.original_snapshot() != expected.original_snapshot
        || RuntimeExecutionFence::from_carrier(&carrier) != expected.execution
        || frame.header.push_watermark != expected.accepted_prefix
        || binding != expected.binding
        || !binding.matches_session_contract(&old)
        || old.track_id.as_str() != expected.track_id
        || old.thread_id != frame.header.last_thread_id
        || abandonment_request_tx(tx, &carrier, mode).await?.as_ref() != Some(request)
        || carrier_ingress_revision_tx(
            tx,
            &expected.card_id,
            &expected.track_id,
            &expected.worker_session_id,
        )
        .await?
            != expected.ingress
    {
        return Err(CalmError::Conflict(
            "recovery carrier changed or still has pending work",
        ));
    }
    super::super::provider_execution::require_unclaimed(tx, &expected.worker_session_id).await?;
    expected.owner.recheck_tx(tx, &carrier).await?;
    if let CallerRevision::Compare(revision) = expected.revision {
        revision.recheck_tx(tx, &carrier).await?;
    }
    // Owner callbacks may read facts, but cannot invalidate the captured ingress.
    if carrier_ingress_revision_tx(
        tx,
        &expected.card_id,
        &expected.track_id,
        &expected.worker_session_id,
    )
    .await?
        != expected.ingress
    {
        return Err(CalmError::Conflict("recovery ingress changed"));
    }
    // Human pending references are card-scoped. Validate through the existing claim owner
    // without minting a plan/claim; System and retry intents retain worker-bound authority.
    if let Some(count) = std::num::NonZeroU32::new(
        u32::try_from(carrier.checkpoint().pending.len())
            .map_err(|_| CalmError::Conflict("pending prefix too large"))?,
    ) {
        let plan = pending_plan_for_prefix_tx(tx, &carrier, count, vec![]).await?;
        super::claim_validation::validate_claim_tx(
            tx,
            &carrier,
            &plan,
            &carrier.checkpoint().pending,
        )
        .await?;
    }
    let mut replacement =
        super::carrier_replacement::replace_verified_carrier_tx(tx, expected, binding, true)
            .await?;
    let mut fresh =
        current_input_carrier_tx(tx, &replacement.card_id, replacement.new_worker.id.as_str())
            .await?;
    fresh.checkpoint.pending = carrier.checkpoint().pending.clone();
    fresh.checkpoint.next_queue_position = carrier.checkpoint().next_queue_position;
    super::carrier::save_input_carrier_tx(tx, &mut fresh).await?;
    replacement.new_worker.handle_state_json = Some(fresh.snapshot().clone());
    if replacement.new_binding.profile == calm_types::harness::HarnessProfile::Planner {
        if replacement.root_session_id.as_deref() != Some(replacement.old_worker.id.as_str()) {
            return Err(CalmError::Conflict("planner root changed"));
        }
        replacement.repoint_root_tx(tx).await?;
    }
    sqlx::query(concat!("INSERT INTO ","conversation_abandonments(card_id,principal,request_key,request_json,old_worker,new_worker,created_a","t_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)"))
        .bind(&replacement.card_id).bind(principal).bind(key).bind(serde_json::to_string(request)?)
        .bind(replacement.old_worker.id.as_str()).bind(replacement.new_worker.id.as_str()).bind(crate::model::now_ms())
        .execute(&mut **tx).await?;
    Ok(replacement)
}
