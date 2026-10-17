//! The shared source decision at claim AND every fresh crossing. The returned
//! provenance/runtime are facts, never a reusable permission for a later write.
use super::*;
use calm_types::conversation::{
    ConversationExecutionProvenance, ConversationHistoryContract, MessageAuthor,
    SubmissionRenderInput, SubmissionRenderSegment,
};
use calm_types::worker::WorkerSession;

pub(crate) async fn authorize_human_execution_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
    plan: &SubmissionRenderPlan,
) -> Result<(ConversationExecutionProvenance, WorkerSession)> {
    let principal = source.current_principal()?;
    let area = conversation::conversation_area_tx(tx, carrier.card_id()).await?;
    crate::organization_access::require_legacy_input_execution_tx(tx, area.id.as_str(), &principal)
        .await?;
    if conversation::history_contract_tx(tx, carrier.card_id()).await?
        != ConversationHistoryContract::Private
    {
        return Err(CalmError::Forbidden(
            "legacy deployment execution cannot run SharedHistory".into(),
        ));
    }
    plan.input_refs()
        .map_err(|error| CalmError::Conflict(error.into()))?;
    for segment in plan.segments() {
        let SubmissionRenderSegment::Input { input, .. } = segment else {
            continue;
        };
        let SubmissionRenderInput::Human { message, author } = input else {
            return Err(CalmError::Forbidden(
                "human execution source cannot authorize machine or legacy input".into(),
            ));
        };
        let stored = conversation::message_tx(tx, &message.message_id).await?;
        if stored.card_id != carrier.card_id()
            || stored.current != *message
            || stored.withdrawn_at_ms.is_some()
            || author.registered_user_id != principal.user_id
            || stored.author
                != (MessageAuthor::AuthenticatedHuman {
                    registered_user_id: principal.user_id.clone(),
                })
        {
            return Err(CalmError::Forbidden(
                "execution source does not own this exact human input".into(),
            ));
        }
    }
    let (binding, runtime) = crate::harness::profile::input_carrier_binding_tx(tx, carrier).await?;
    Ok((
        ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: principal.user_id,
            provider: binding.provider,
            profile: binding.profile,
        },
        runtime,
    ))
}

pub(crate) async fn claim_human_initial_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &mut CurrentInputCarrier,
    plan: SubmissionRenderPlan,
) -> Result<ClaimedSubmission> {
    let (execution, _) = authorize_human_execution_tx(tx, source, carrier, &plan).await?;
    Ok(conversation::claim_initial_submission_tx(tx, carrier, plan, &execution).await?)
}
