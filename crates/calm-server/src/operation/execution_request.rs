//! The original one-shot executor request belongs to the Operation, independently
//! of its mutable domain output. Execution status remains in the executor journal.

use calm_session::control::execution::{AttachRegistration, PrepareExecution};
use serde::{Deserialize, Serialize};

use super::{
    Operation, Tx, admission::require_live_claim_tx, repo_sqlite::require_original_operation_tx,
};
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    error::{CalmError, Result},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalExecutionRequest {
    /// Workspace-owned continuity anchor, separate from this operation's lease.
    pub(crate) source_anchor: AttachRegistration,
    pub(crate) registration: AttachRegistration,
    pub(crate) request: PrepareExecution,
}

pub(crate) async fn load_tx(
    tx: &mut Tx<'_>,
    original: &Operation,
) -> Result<Option<OriginalExecutionRequest>> {
    require_original_operation_tx(tx, original).await?;
    let json: Option<String> =
        sqlx::query_scalar("SELECT execution_request_json FROM operations WHERE id=?1")
            .bind(&original.id)
            .fetch_one(&mut **tx)
            .await?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

pub(crate) async fn load(
    repo: &dyn RouteRepo,
    original: &Operation,
) -> Result<Option<OriginalExecutionRequest>> {
    let original = original.clone();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { load_tx(tx, &original).await })
    })
    .await
}

/// A single write under the same real claim and accepted grant. This function
/// cannot replace a previous request or manufacture an unleased operation.
pub(crate) async fn reserve_tx(
    tx: &mut Tx<'_>,
    original: &Operation,
    request: &OriginalExecutionRequest,
) -> Result<()> {
    require_live_claim_tx(tx, original).await?;
    if request.request.identity.operation_id != original.id {
        return Err(CalmError::Conflict(
            "executor request belongs to another original operation".into(),
        ));
    }
    let grant: Option<String> = sqlx::query_scalar("SELECT grant_id FROM operation_execution_grants WHERE operation_id=?1 AND original_payload_hash=?2")
        .bind(&original.id).bind(&original.payload_hash).fetch_optional(&mut **tx).await?;
    if grant.as_deref() != Some(request.request.identity.authority.grant_id.as_str()) {
        return Err(CalmError::Conflict(
            "executor request has no matching original accepted grant".into(),
        ));
    }
    let changed = sqlx::query("UPDATE operations SET execution_request_json=?1 WHERE id=?2 AND execution_request_json IS NULL")
        .bind(serde_json::to_string(request)?).bind(&original.id).execute(&mut **tx).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(
            "original executor request is already reserved".into(),
        ));
    }
    Ok(())
}
