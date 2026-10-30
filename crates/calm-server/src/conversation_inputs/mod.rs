//! Source admission for canonical human work. Principal identity comes from the
//! real session issuer/middleware; Actor labels and profile roles are audit data.
//! Canonical ingress checks this owner in its protected transaction. Key replay
//! must check it before retrieving a stored result. Storage retains factual rules.
mod execution;
pub(crate) mod pending_controls;
pub(crate) mod resolution;
mod source;
pub(crate) use source::{
    InputSource, SourceInputs, authorize_execution_tx, authorize_resource_tx, claim_initial_tx,
    source_inputs_tx,
};
mod historical;
pub(crate) use historical::HistoricalHumanSource;
mod storage_contract;
pub(crate) use storage_contract::{
    InputStorageContract, has_canonical_storage, human_input_storage_contract,
    require_historical_storage, require_historical_storage_tx,
    require_recreatable_track_carriers_tx,
};
mod selection_read;
pub(crate) use selection_read::SelectionReadAdmission;

#[cfg(all(test, feature = "fixtures"))]
mod tests;
pub(crate) use execution::authorize_human_resource_tx;

use calm_types::conversation::{HumanPayload, MessageRevisionRef, SubmissionRenderPlan};
use calm_types::organization::AreaAccess;
use sqlx::{Sqlite, Transaction};

use crate::auth::{HumanInputSource, Principal};
use crate::db::sqlite::conversation::{
    self, AcceptedPendingInput, ClaimedSubmission, CurrentInputCarrier,
};
use crate::error::{CalmError, Result};

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
