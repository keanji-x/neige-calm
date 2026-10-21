//! Initial submissions freeze through the canonical claim/storage path.
//! The server's canonical transaction owner authenticates the live caller or
//! kernel context before calling these functions. Provenance alone is no grant.
use super::carrier::save_input_carrier_tx;
use super::claim_storage::insert_submission_tx;
use super::claim_validation::validate_claim_tx;
use super::{CurrentInputCarrier, history_contract_tx};
use crate::error::{CalmError, Result};
use crate::model::new_id;
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

/// The claim owner selects ordinary input provenance or one explicit intent.
#[derive(Clone)]
pub enum InitialClaim {
    PendingPrefix {
        plan: SubmissionRenderPlan,
    },
    ExplicitSuccessor {
        retry_intent_id: calm_types::conversation::RetryIntentId,
    },
}
impl From<SubmissionRenderPlan> for InitialClaim {
    fn from(plan: SubmissionRenderPlan) -> Self {
        Self::PendingPrefix { plan }
    }
}

/// Claims only an exact prefix of the current sole queue; inputs accepted after
/// the caller selected its batch remain untouched in that queue. All authority
/// checks and this call must share the caller's protected immediate transaction.
pub async fn claim_initial_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    selection: impl Into<InitialClaim>,
    provenance: &ConversationExecutionProvenance,
) -> Result<ClaimedSubmission> {
    carrier.require_execution_current()?;
    let resolved = carrier.checkpoint.resolved_execution_without_target();
    if (!matches!(
        carrier.phase()?,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
    ) && !(resolved && carrier.phase()? == HarnessPhaseTag::IssuingTurn))
        || (carrier.checkpoint.active.is_some() && !resolved)
    {
        return Err(CalmError::Conflict(
            "conversation cannot claim another Initial submission now",
        ));
    }
    require_private_tx(tx, &carrier.card_id).await?;
    let (plan, entries, retry) = match selection.into() {
        InitialClaim::PendingPrefix { plan } => {
            let refs = plan.input_refs().map_err(CalmError::Conflict)?;
            if refs.len() > carrier.checkpoint.pending.len() {
                return Err(CalmError::Conflict("pending batch changed before claim"));
            }
            let entries = carrier.checkpoint.pending[..refs.len()].to_vec();
            validate_claim_tx(tx, carrier, &plan, &entries).await?;
            (plan, entries, None)
        }
        InitialClaim::ExplicitSuccessor { retry_intent_id } => {
            let intent = super::retry::retry_intent_tx(tx, carrier, &retry_intent_id).await?;
            if intent.unavailable.is_some()
                || carrier.checkpoint.pending.first() != Some(&intent.pending)
            {
                return Err(CalmError::Conflict(
                    "retry intent is not the exact pending head",
                ));
            }
            (
                intent.plan.plan.clone(),
                vec![intent.pending.clone()],
                Some((retry_intent_id, intent)),
            )
        }
    };
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        deployment_owner_user_id,
        provider,
        profile: _,
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
    super::claim_storage::insert_turn_tx(
        tx,
        &carrier.card_id,
        &carrier.worker_session_id,
        &logical_turn_id,
        &submission_id,
        provenance,
    )
    .await?;
    let original_digest = match retry {
        None => {
            insert_submission_tx(
                tx,
                &carrier.card_id,
                &logical_turn_id,
                &submission_id,
                &plan,
                &entries,
            )
            .await?
        }
        Some((id, intent)) => {
            super::claim_storage::insert_retry_submission_tx(
                tx,
                &carrier.card_id,
                &logical_turn_id,
                &submission_id,
                &id,
                &intent,
            )
            .await?
        }
    };
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
