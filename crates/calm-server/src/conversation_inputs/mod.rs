//! Source admission for canonical human work. Principal identity comes from the
//! real session issuer/middleware; Actor labels and profile roles are audit data.
//! Canonical ingress checks this owner in its protected transaction. Key replay
//! must check it before retrieving a stored result. Storage retains factual rules.
mod execution;
mod selection_read;
pub(crate) use selection_read::SelectionReadAdmission;
#[cfg(all(test, feature = "fixtures"))]
mod tests;
pub(crate) use execution::{
    authorize_human_execution_tx, authorize_human_resource_tx, claim_human_initial_tx,
};

use calm_types::conversation::{
    HumanPayload, LogicalTurnId, MessageRevisionRef, PrivateHistoryOperationId,
    SubmissionRenderPlan,
};
use calm_types::organization::AreaAccess;
use sqlx::{Sqlite, Transaction};

use crate::auth::{HumanInputSource, Principal};
use crate::db::sqlite::conversation::{
    self, AcceptedPendingInput, ClaimedSubmission, CurrentInputCarrier,
};
use crate::error::{CalmError, Result};
use crate::harness::PlannerBackend;

pub(crate) async fn authorize_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &Principal,
    card_id: &str,
) -> Result<crate::model::Area> {
    let area = conversation::conversation_area_tx(tx, card_id).await?;
    crate::organization_access::require_area_access_tx(
        tx,
        area.id.as_str(),
        principal,
        AreaAccess::Collaborate,
    )
    .await
}

pub(crate) async fn accept_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &Principal,
    carrier: &mut CurrentInputCarrier,
    payload: HumanPayload,
) -> Result<AcceptedPendingInput> {
    authorize_human_tx(tx, principal, carrier.card_id()).await?;
    Ok(conversation::accept_pending_human_tx(tx, carrier, &principal.user_id, payload).await?)
}

pub(crate) async fn edit_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &Principal,
    carrier: &mut CurrentInputCarrier,
    expected: &MessageRevisionRef,
    payload: HumanPayload,
) -> Result<MessageRevisionRef> {
    authorize_human_tx(tx, principal, carrier.card_id()).await?;
    Ok(
        conversation::edit_pending_human_tx(tx, carrier, &principal.user_id, expected, payload)
            .await?,
    )
}

pub(crate) async fn withdraw_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    principal: &Principal,
    carrier: &mut CurrentInputCarrier,
    expected: &MessageRevisionRef,
    operation_id: &PrivateHistoryOperationId,
) -> Result<()> {
    authorize_human_tx(tx, principal, carrier.card_id()).await?;
    Ok(conversation::withdraw_pending_human_tx(
        tx,
        carrier,
        &principal.user_id,
        expected,
        operation_id,
    )
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn claim_human_supplemental_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    carrier: &mut CurrentInputCarrier,
    backend: &PlannerBackend,
    logical_turn_id: &LogicalTurnId,
    expected_native_turn_id: &str,
    plan: SubmissionRenderPlan,
) -> Result<ClaimedSubmission> {
    let (execution, _) = authorize_human_execution_tx(tx, source, carrier, &plan).await?;
    let calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id,
        provider,
        ..
    } = execution;
    if provider != backend.provider() || !backend.supports_steer() {
        return Err(CalmError::Forbidden(
            "this provider does not support Supplemental input".into(),
        ));
    }
    Ok(conversation::claim_supplemental_submission_tx(
        tx,
        carrier,
        &deployment_owner_user_id,
        logical_turn_id,
        expected_native_turn_id,
        plan,
    )
    .await?)
}
