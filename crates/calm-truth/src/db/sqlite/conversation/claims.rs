//! Initial submissions freeze through the canonical claim/storage path.
//! The server's canonical transaction owner authenticates the live caller or
//! kernel context before calling these functions. Provenance alone is no grant.
use super::carrier::save_input_carrier_tx;
use super::claim_storage::insert_submission_tx;
use super::claim_validation::validate_claim_tx;
use super::{CurrentInputCarrier, history_contract_tx};
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};
use calm_types::conversation::{
    ActiveInputSubmissions, ConversationExecutionProvenance, ConversationHistoryContract,
    InputSubmissionId, LogicalTurnId, SubmissionRenderDigest, SubmissionRenderPlan,
};
use calm_types::harness::HarnessPhaseTag;
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedSubmission {
    pub submission_id: InputSubmissionId,
    pub logical_turn_id: LogicalTurnId,
    pub ordinal: u32,
    pub plan: SubmissionRenderPlan,
    pub original_digest: SubmissionRenderDigest,
    pub snapshot_after_write: serde_json::Value,
}

/// Claims only an exact prefix of the current sole queue; inputs accepted after
/// the caller selected its batch remain untouched in that queue. All authority
/// checks and this call must share the caller's protected immediate transaction.
pub async fn claim_initial_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    plan: SubmissionRenderPlan,
    provenance: &ConversationExecutionProvenance,
) -> Result<ClaimedSubmission> {
    carrier.require_execution_current()?;
    let kept = carrier.checkpoint.kept_execution_without_target();
    if (!matches!(
        carrier.phase()?,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
    ) && !(kept && carrier.phase()? == HarnessPhaseTag::IssuingTurn))
        || (carrier.checkpoint.active.is_some() && !kept)
    {
        return Err(CalmError::Conflict(
            "conversation cannot claim another Initial submission now",
        ));
    }
    require_private_tx(tx, &carrier.card_id).await?;
    let refs = plan.input_refs().map_err(CalmError::Conflict)?;
    if refs.len() > carrier.checkpoint.pending.len() {
        return Err(CalmError::Conflict("pending batch changed before claim"));
    }
    let entries = carrier.checkpoint.pending[..refs.len()].to_vec();
    validate_claim_tx(tx, carrier, &plan, &entries).await?;
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id,
        provider,
        profile,
    } = provenance;
    super::super::area_membership::application_user_require_registered(
        tx,
        deployment_owner_user_id,
    )
    .await?;
    let runtime_provider: String =
        sqlx::query_scalar("SELECT provider FROM worker_sessions WHERE id=?1")
            .bind(&carrier.worker_session_id)
            .fetch_one(&mut **tx)
            .await?;
    if runtime_provider != provider.wire_name() {
        return Err(CalmError::Conflict(
            "execution provenance disagrees with the current provider",
        ));
    }
    let submission_id = InputSubmissionId::parse(&new_id())
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    let logical_turn_id =
        LogicalTurnId::parse(&new_id()).map_err(|error| CalmError::Internal(error.to_string()))?;
    let profile = serde_json::to_value(profile)?;
    sqlx::query("INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
         deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES(?1,?2,?3,?4,'legacy_single_user_deployment',?5,?6,?7,?8)")
        .bind(logical_turn_id.as_str()).bind(&carrier.card_id).bind(&carrier.worker_session_id).bind(submission_id.as_str())
        .bind(deployment_owner_user_id).bind(provider.wire_name()).bind(profile.as_str().ok_or_else(|| CalmError::Internal("invalid persisted harness profile".into()))?)
        .bind(now_ms()).execute(&mut **tx).await?;
    let original_digest = insert_submission_tx(
        tx,
        &carrier.card_id,
        &logical_turn_id,
        &submission_id,
        &plan,
        &entries,
    )
    .await?;
    carrier.checkpoint.pending.drain(..entries.len());
    carrier.checkpoint.active = Some(ActiveInputSubmissions {
        logical_turn_id: logical_turn_id.clone(),
        submission_ids: vec![submission_id.clone()],
    });
    carrier.set_phase(HarnessPhaseTag::IssuingTurn)?;
    save_input_carrier_tx(tx, carrier).await?;
    Ok(ClaimedSubmission {
        submission_id,
        logical_turn_id,
        ordinal: 0,
        plan,
        original_digest,
        snapshot_after_write: carrier.snapshot.clone(),
    })
}

pub(super) async fn require_private_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    if history_contract_tx(tx, card_id).await? != ConversationHistoryContract::Private {
        return Err(CalmError::Forbidden("SharedHistory execution requires its real Area execution grant; legacy deployment execution is unavailable".into()));
    }
    Ok(())
}
