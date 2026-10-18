//! The shared source decision at claim AND every fresh crossing. The returned
//! provenance/runtime are facts, never a reusable permission for a later write.
use super::*;
use calm_types::conversation::{
    AcceptedInputRef, ConversationExecutionProvenance, MessageAuthor, SubmissionRenderInput,
    SubmissionRenderSegment,
};
use calm_types::worker::WorkerSession;

/// The shared current resource decision before a new private effect. Success
/// is not a reusable grant: the owning task keeps the real source and calls
/// this decision again after awaits before beginning another effect.
pub(crate) async fn authorize_human_resource_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
) -> Result<()> {
    let area = conversation::conversation_area_tx(tx, carrier.card_id()).await?;
    authorize_legacy_human_area_tx(tx, source, area.id.as_str()).await
}

/// The same private deployment resource policy is used by canonical execution
/// and historical public effects. Content acceptance has its separate ACL.
pub(super) async fn authorize_legacy_human_area_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    area_id: &str,
) -> Result<()> {
    let principal = source.current_principal()?;
    crate::organization_access::require_legacy_input_execution_tx(tx, area_id, &principal).await?;
    if conversation::area_history_contract_tx(tx, area_id).await?
        != calm_types::conversation::ConversationHistoryContract::Private
    {
        return Err(CalmError::Forbidden(
            "legacy deployment execution cannot run SharedHistory".into(),
        ));
    }
    Ok(())
}

/// The one current-author/revision rule used by startup and input claims.
async fn current_human_ref_matches_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
    message: &MessageRevisionRef,
) -> Result<bool> {
    let principal = source.current_principal()?;
    let stored = conversation::message_tx(tx, &message.message_id).await?;
    if stored.card_id != carrier.card_id()
        || stored.current != *message
        || stored.withdrawn_at_ms.is_some()
        || stored.author
            != (MessageAuthor::AuthenticatedHuman {
                registered_user_id: principal.user_id,
            })
    {
        return Ok(false);
    }
    Ok(matches!(
        conversation::revision_tx(tx, message).await?.payload,
        calm_types::conversation::PrivateHistoryPayload::Present { .. }
    ))
}

/// One current request's body-free associations, derived together by this owner.
/// This cannot be serialized or constructed from a recorded author or receipt.
pub(crate) struct HumanSourceInputs {
    refs: Vec<MessageRevisionRef>,
    unattempted_initial: bool,
}

impl HumanSourceInputs {
    pub(crate) fn refs(&self) -> &[MessageRevisionRef] {
        &self.refs
    }

    pub(crate) fn is_unattempted_initial(&self) -> bool {
        self.unattempted_initial
    }
}

/// Re-admit the original claim before any crossing, then the owned pending
/// prefix. Already attempted work never gains another dispatch from this lookup.
pub(crate) async fn human_source_inputs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
) -> Result<HumanSourceInputs> {
    let mut refs = vec![];
    let mut unattempted_initial = false;
    if carrier.phase()? == crate::harness::HarnessPhaseTag::IssuingTurn
        && let Some(active) = &carrier.checkpoint().active
        && let [id] = active.submission_ids.as_slice()
        && carrier.checkpoint().attempt(id).is_none()
    {
        let original = conversation::submission_identity_tx(tx, id).await?;
        let stored = conversation::submission_plan_tx(tx, id).await?;
        let (execution, _) =
            authorize_human_execution_tx(tx, source, carrier, &stored.plan).await?;
        if original.card_id != carrier.card_id()
            || original.logical_turn_id != active.logical_turn_id
            || original.ordinal != 0
            || original.execution != execution
        {
            return Err(CalmError::Conflict(
                "original unattempted claim changed".into(),
            ));
        }
        let original_refs = stored
            .plan
            .input_refs()
            .map_err(|error| CalmError::Conflict(error.into()))?;
        let [AcceptedInputRef::Human { message }] = original_refs.as_slice() else {
            return Err(CalmError::Conflict(
                "original claim is not one Human input".into(),
            ));
        };
        refs.push(message.clone());
        unattempted_initial = true;
    }
    for pending in &carrier.checkpoint().pending {
        let calm_types::conversation::PendingInput::Human { message, .. } = pending else {
            break;
        };
        if !current_human_ref_matches_tx(tx, source, carrier, message).await? {
            break;
        }
        refs.push(message.clone());
    }
    if refs.is_empty() {
        return Err(CalmError::Forbidden(
            "current source cannot resume this exact pending head".into(),
        ));
    }
    Ok(HumanSourceInputs {
        refs,
        unattempted_initial,
    })
}

pub(crate) async fn authorize_human_execution_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &CurrentInputCarrier,
    plan: &SubmissionRenderPlan,
) -> Result<(ConversationExecutionProvenance, WorkerSession)> {
    authorize_human_resource_tx(tx, source, carrier).await?;
    let principal = source.current_principal()?;
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
        if author.registered_user_id != principal.user_id
            || !current_human_ref_matches_tx(tx, source, carrier, message).await?
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
