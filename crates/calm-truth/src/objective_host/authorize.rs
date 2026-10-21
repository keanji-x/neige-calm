use super::*;
use crate::decision_gate::{PrincipalDecisionGate, WriteTx};

/// Private fields bind the capability to this exact input. No retargeting accessor.
pub(super) struct AuthorizedInput<'a> {
    input: &'a ObjectiveInput,
}
impl<'a> AuthorizedInput<'a> {
    pub(super) fn input(&self) -> &'a ObjectiveInput {
        self.input
    }
}

pub(super) async fn authorize<'a>(
    tx: &mut Transaction<'_, Sqlite>,
    input: &'a ObjectiveInput,
) -> Result<AuthorizedInput<'a>> {
    capability::require_tx(tx, &input.track_id).await?;
    let principal = match (&input.event, &input.source) {
        (Event::DefinitionChanged { .. }, InputSource::ReportBlock { .. }) => {
            report::verify_source_tx(tx, input).await?;
            input.source.identity()?;
            return Ok(AuthorizedInput { input });
        }
        (Event::CheckFinished { .. }, InputSource::IntentJob { .. }) => {
            input.source.identity()?;
            return Ok(AuthorizedInput { input });
        }
        (
            Event::Requested { .. } | Event::Settled { .. } | Event::Closed { .. },
            InputSource::Planner { principal, .. },
        ) => principal.clone(),
        (
            Event::HostIdle { .. } | Event::Usage { .. },
            InputSource::HarnessTurn { session_id, .. },
        ) => Principal::Agent {
            session_id: session_id.clone(),
            track_id: input.track_id.clone(),
            area_id: tx
                .read_track_area(&input.track_id)
                .await?
                .ok_or_else(|| TruthError::Forbidden("objective track missing".into()))?,
        },
        (
            Event::DefinitionChanged { .. }
            | Event::Requested { .. }
            | Event::Settled { .. }
            | Event::Closed { .. }
            | Event::HostIdle { .. }
            | Event::Usage { .. },
            _,
        )
        | (
            Event::ApprovalGranted { .. }
            | Event::RevisionDelivered { .. }
            | Event::CheckFinished { .. },
            _,
        ) => {
            return Err(TruthError::Forbidden(
                "objective source cannot supply this event".into(),
            ));
        }
    };
    PrincipalDecisionGate::new(principal.clone())
        .decide_planner(tx, &input.track_id)
        .await?
        .into_result()?;
    // Role/card containment is shared gate policy; the objective feed additionally
    // binds to the currently selected session and the session's persisted track.
    let Principal::Agent { session_id, .. } = principal else {
        unreachable!("planner gate denied non-agent")
    };
    let session = tx
        .read_worker_session(&session_id)
        .await?
        .ok_or_else(|| TruthError::Forbidden("objective session missing".into()))?;
    let card = session
        .card_id
        .ok_or_else(|| TruthError::Forbidden("objective card missing".into()))?;
    let binding = tx.read_card_binding(&card).await?;
    if session.track_id != input.track_id
        || !binding.is_some_and(|(_, _, current)| current.as_deref() == Some(session_id.as_str()))
    {
        return Err(TruthError::Forbidden(
            "objective session is no longer selected for this track".into(),
        ));
    }
    input.source.identity()?;
    Ok(AuthorizedInput { input })
}

#[cfg(test)]
pub(super) fn fixture(input: &ObjectiveInput) -> AuthorizedInput<'_> {
    AuthorizedInput { input }
}
