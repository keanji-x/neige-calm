//! Canonical retained recovery. Receipt identity comes from the Initial's
//! checkpoint/proof, never a latest-row heuristic or the historical queue.
use super::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{AcpCanonicalInput, AcpSubmissionInput, AcpSubmissionState};
use calm_types::conversation::{InputAttemptFact, InputCorrelationFact, InputDeliveryFact};

impl AcpPlannerSession {
    /// Called under the retained install reservation, after the predecessor has
    /// left the registry. Opening a control object alone never runs recovery.
    pub(crate) async fn recover_retained(&self) -> Result<()> {
        let shared = &self.shared;
        let _issue = shared.issue.lock().await;
        if slot_exists(shared) {
            return Ok(());
        }
        let params = &shared.params;
        let (card, worker, provider) = (
            params.card_id.clone(),
            params.worker_session_id.clone(),
            params.provider.clone(),
        );
        let found = write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move { canonical_receipt_tx(tx, &card, &worker, &provider).await })
        })
        .await?;
        // Completed is persisted only after the original writer's confirmed
        // stop/revoke. Re-establish cleanup for this recovery process before
        // retaining that exact durable result; Sending/Unknown never qualify.
        let revoked = revoke(params).await;
        let stopped = self
            .stop_execution(crate::provider_execution::StopReason::Shutdown)
            .await;
        revoked?;
        stopped?;
        let Some((receipt, association)) = found else {
            return Ok(());
        };
        if !matches!(receipt.state, AcpSubmissionState::Completed) {
            return Ok(());
        }
        let mut outcome: Value =
            serde_json::from_str(receipt.outcome_json.as_deref().ok_or_else(|| {
                CalmError::Conflict("ACP completed receipt has no outcome".into())
            })?)?;
        if outcome.get("id").and_then(Value::as_str) != Some(receipt.turn_id.as_str()) {
            return Err(CalmError::Conflict(
                "ACP receipt terminal names another turn".into(),
            ));
        }
        crate::harness::turn_outcome::canonical_diagnostic(&mut outcome);
        crate::harness::turn_outcome::record(
            params.repo.as_ref(),
            &params.worker_session_id,
            &params.card_id,
            &params.track_id,
            &receipt.thread_id,
            &receipt.turn_id,
            &outcome,
            crate::harness::turn_outcome::Confirmation::ProviderTerminal,
        )
        .await?;
        execution_end::install(shared, DriverSlot::recovered(receipt, association));
        Ok(())
    }
}

fn slot_exists(shared: &Shared) -> bool {
    shared.driver.lock().expect("ACP driver").is_some()
}

pub(super) async fn canonical_receipt_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    worker: &str,
    provider: &AgentProvider,
) -> Result<Option<(AcpSubmission, AcpCanonicalInput)>> {
    let mut carrier = current_input_fact_carrier_tx(tx, card, worker).await?;
    let (binding, row) = crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
    if binding.provider != *provider {
        return Err(CalmError::Conflict("ACP recovery binding changed".into()));
    }
    let active = carrier.checkpoint().active.as_ref();
    let initial: Option<String> = match active {
        Some(_) => original_initial_submission_tx(tx, &carrier)
            .await?
            .map(|id| id.as_str().to_owned()),
        None => {
            let turn = carrier
                .snapshot()
                .get("last_turn_id")
                .and_then(Value::as_str);
            sqlx::query_scalar(
                "SELECT client_id FROM acp_submissions WHERE worker_session_id=?1 AND turn_id=?2",
            )
            .bind(worker)
            .bind(turn)
            .fetch_optional(&mut **tx)
            .await?
        }
    };
    let Some(initial) = initial else {
        return Ok(None);
    };
    let Some(receipt) = acp_submission_get(&mut **tx, worker, &initial).await? else {
        return Ok(None);
    };
    let AcpSubmissionInput::Canonical(association) =
        AcpSubmissionInput::decode(&receipt.input_json)?
    else {
        return Err(CalmError::Conflict(
            "canonical ACP recovery found a historical receipt".into(),
        ));
    };
    let attempt = association.proof.attempt();
    let stored = submission_plan_tx(tx, &attempt.submission_id).await?;
    let identity = submission_identity_tx(tx, &attempt.submission_id).await?;
    if receipt.client_id != initial
        || attempt.submission_id.as_str() != initial
        || identity.card_id != card
        || identity.logical_turn_id != association.logical_turn_id
        || active.is_some_and(|active| {
            association.logical_turn_id != active.logical_turn_id
                || !active.submission_ids.contains(&attempt.submission_id)
        })
        || carrier
            .checkpoint()
            .attempt(&attempt.submission_id)
            .is_none_or(|fact| fact.attempt != *attempt)
        || association.proof.render_plan().sha256 != stored.original_digest
        || association.proof.input_refs()
            != stored
                .plan
                .input_refs()
                .map_err(|error| CalmError::Conflict(error.into()))?
        || carrier
            .snapshot()
            .get("last_thread_id")
            .and_then(Value::as_str)
            != Some(receipt.thread_id.as_str())
        || row.thread_id.as_deref() != Some(receipt.thread_id.as_str())
        || row.agent_session_id.as_deref() != Some(receipt.native_session_id.as_str())
    {
        return Err(CalmError::Conflict(
            "ACP recovery receipt association changed".into(),
        ));
    }
    let fact = carrier
        .checkpoint()
        .attempt(&attempt.submission_id)
        .expect("checked attempt");
    match fact.delivery {
        InputDeliveryFact::CrossingUnknown => {
            // Recover a lost private report through the existing fact owner.
            // This refines correlation only; receipt existence never proves delivery.
            record_input_attempt_fact_tx(
                tx,
                &mut carrier,
                InputAttemptFact {
                    attempt: attempt.clone(),
                    correlation: InputCorrelationFact {
                        provider: provider.clone(),
                        thread_id: receipt.thread_id.clone(),
                        native_session_id: Some(receipt.native_session_id.clone()),
                        native_turn_id: None,
                        local_turn_id: Some(receipt.turn_id.clone()),
                        rpc_request_id: None,
                    },
                    delivery: InputDeliveryFact::Unknown,
                },
            )
            .await?;
        }
        InputDeliveryFact::Unknown | InputDeliveryFact::Accepted { .. } => {
            if fact.correlation.provider != *provider
                || fact.correlation.thread_id != receipt.thread_id
                || fact.correlation.local_turn_id.as_deref() != Some(receipt.turn_id.as_str())
                || fact.correlation.native_session_id.as_deref()
                    != Some(receipt.native_session_id.as_str())
                || fact.correlation.native_turn_id.is_some()
            {
                return Err(CalmError::Conflict(
                    "ACP recovery correlation changed".into(),
                ));
            }
        }
        InputDeliveryFact::NotDispatched { .. } => {
            return Err(CalmError::Conflict(
                "ACP receipt contradicts dispatch refusal".into(),
            ));
        }
    }
    Ok(Some((receipt, association)))
}
