//! Terminal persistence after the caller has admitted its claimed or parked authority.
use super::*;
use crate::event::{Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::ActorId;

/// The owner supplies every event fact; generic completion infers neither actor nor scope.
#[derive(Clone, Debug)]
pub struct OwnerEvent {
    pub actor: ActorId,
    pub scope: EventScope,
    pub event: Event,
}

pub(crate) async fn append_owner_event_tx(
    tx: &mut super::Tx<'_>,
    owner: OwnerEvent,
) -> Result<BroadcastEnvelope> {
    let OwnerEvent {
        actor,
        scope,
        event,
    } = owner;
    let id =
        crate::db::sqlite::append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
    Ok(BroadcastEnvelope {
        id,
        event_version: SYNC_EVENT_VERSION,
        actor,
        scope,
        event,
    })
}

/// A terminal decision, separate from a collision receipt for an existing operation.
#[derive(Clone, Debug)]
pub enum TerminalOutcome {
    Succeeded,
    SucceededWithResult {
        result: Value,
    },
    Failed {
        last_error: String,
        from_phase: PhaseTag,
        last_error_class: Option<String>,
    },
    Stuck {
        reason: String,
        from_phase: PhaseTag,
    },
}

impl From<&ParkedOutcome> for TerminalOutcome {
    fn from(outcome: &ParkedOutcome) -> Self {
        match outcome {
            ParkedOutcome::Succeeded { result } => Self::SucceededWithResult {
                result: result.clone(),
            },
            ParkedOutcome::Failed {
                last_error,
                last_error_class,
            } => Self::Failed {
                last_error: last_error.clone(),
                from_phase: PhaseTag::Parked,
                last_error_class: last_error_class.clone(),
            },
        }
    }
}

/// Caller must validate its authority in this same writer transaction. No admission
/// mode is allowed to silently fall back to another when its fence loses.
pub(super) async fn write_terminal_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    outcome: &TerminalOutcome,
) -> Result<OperationResult> {
    let now = crate::model::now_ms();
    // Read the current output: owner checkpoints may have advanced since the claim.
    let raw: Option<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
            .bind(&op.id)
            .fetch_one(&mut **tx)
            .await?;
    let mut output: Option<TxOutput> = raw.as_deref().map(serde_json::from_str).transpose()?;
    if let TerminalOutcome::SucceededWithResult { result } = outcome {
        let output = output
            .as_mut()
            .ok_or_else(|| CalmError::Internal("successful operation has no output".into()))?;
        output.result = result.clone();
    }
    if let Some(output) = &mut output {
        output.post_commit_events.clear();
    }
    let (phase, detail, error, result) = match outcome {
        TerminalOutcome::Succeeded | TerminalOutcome::SucceededWithResult { .. } => (
            "succeeded",
            None,
            None,
            OperationOutcome::Succeeded {
                result: output
                    .as_ref()
                    .ok_or_else(|| {
                        CalmError::Internal("successful operation has no output".into())
                    })?
                    .result
                    .clone(),
            },
        ),
        TerminalOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => (
            "failed",
            Some(
                serde_json::json!({"from_phase": from_phase, "last_error_class": last_error_class}),
            ),
            Some(last_error.as_str()),
            OperationOutcome::Failed {
                last_error: last_error.clone(),
                from_phase: *from_phase,
                last_error_class: last_error_class.clone(),
            },
        ),
        TerminalOutcome::Stuck { reason, from_phase } => (
            "stuck",
            Some(serde_json::json!({"reason": reason, "since": now, "from_phase": from_phase})),
            Some(reason.as_str()),
            OperationOutcome::Stuck {
                reason: reason.clone(),
                from_phase: *from_phase,
            },
        ),
    };
    sqlx::query(
        "UPDATE operations SET phase=?1,phase_detail_json=?2,last_error=?3,tx_output_json=?4, \
         lease_owner=NULL,lease_until_ms=NULL,parked_deadline_ms=NULL,completed_at_ms=?5,updated_at_ms=?5 WHERE id=?6"
    ).bind(phase).bind(detail.as_ref().map(serde_json::to_string).transpose()?)
        .bind(error).bind(output.as_ref().map(serde_json::to_string).transpose()?)
        .bind(now).bind(&op.id).execute(&mut **tx).await?;
    Ok(OperationResult {
        op_id: op.id.clone(),
        outcome: result,
    })
}

/// Both claimed failure and owned parked settlement use the same event selection.
/// Explicit owner events take precedence; the adapter owns all domain policy.
pub(super) async fn append_result_event_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    adapter: &dyn ProviderAdapter,
    outcome: &TerminalOutcome,
    declared: Option<OwnerEvent>,
) -> Result<Option<crate::event::BroadcastEnvelope>> {
    let event = match (declared, outcome) {
        (Some(event), _) => Some(event),
        (None, TerminalOutcome::Succeeded | TerminalOutcome::SucceededWithResult { .. }) => None,
        (None, TerminalOutcome::Failed { last_error, .. }) => {
            adapter.failure_event_tx(tx, op, last_error).await?
        }
        (None, TerminalOutcome::Stuck { reason, .. }) => {
            adapter.failure_event_tx(tx, op, reason).await?
        }
    };
    match event {
        Some(event) => append_owner_event_tx(tx, event).await.map(Some),
        None => Ok(None),
    }
}
