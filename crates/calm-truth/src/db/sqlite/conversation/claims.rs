//! Initial and Supplemental freeze through one canonical claim/storage path.
//! The server's canonical transaction owner authenticates the live caller or
//! kernel context before calling these functions. Provenance alone is no grant.
use super::carrier::save_input_carrier_tx;
use super::claim_storage::insert_submission_tx;
use super::claim_validation::validate_claim_tx;
use super::{CurrentInputCarrier, history_contract_tx, message_tx};
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};
use calm_types::conversation::{
    AcceptedInputRef, ActiveInputSubmissions, ConversationExecutionProvenance,
    ConversationHistoryContract, InputSubmissionId, LogicalTurnId, MessageAuthor,
    SubmissionRenderDigest, SubmissionRenderPlan,
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
    if !matches!(
        carrier.phase()?,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
    ) || carrier.checkpoint.active.is_some()
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
        0,
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

/// Private steer claims an original author's exact pending human revision. The
/// existing running logical/native Turn remains the execution owner; no second
/// Turn is minted and machine/legacy sources cannot enter this path.
pub async fn claim_supplemental_submission_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    author_user_id: &str,
    logical_turn_id: &LogicalTurnId,
    expected_native_turn_id: &str,
    plan: SubmissionRenderPlan,
) -> Result<ClaimedSubmission> {
    require_private_tx(tx, &carrier.card_id).await?;
    if carrier.phase()? != HarnessPhaseTag::TurnRunning
        || carrier
            .snapshot
            .get("last_turn_id")
            .and_then(serde_json::Value::as_str)
            != Some(expected_native_turn_id)
        || carrier
            .checkpoint
            .active
            .as_ref()
            .is_none_or(|active| &active.logical_turn_id != logical_turn_id)
    {
        return Err(CalmError::Conflict(
            "Supplemental target is no longer the current running Turn",
        ));
    }
    let refs = plan.input_refs().map_err(CalmError::Conflict)?;
    let [AcceptedInputRef::Human { message }] = refs.as_slice() else {
        return Err(CalmError::Forbidden(
            "Supplemental requires one original author's pending human input".into(),
        ));
    };
    let current = message_tx(tx, &message.message_id).await?;
    if current.author
        != (MessageAuthor::AuthenticatedHuman {
            registered_user_id: author_user_id.into(),
        })
    {
        return Err(CalmError::Forbidden(
            "only the original human author can steer this input".into(),
        ));
    }
    let position = carrier
        .checkpoint
        .pending
        .iter()
        .position(|entry| entry.input_ref() == refs[0])
        .ok_or_else(|| CalmError::Conflict("human input is no longer pending"))?;
    let entries = vec![carrier.checkpoint.pending[position].clone()];
    validate_claim_tx(tx, carrier, &plan, &entries).await?;
    let previous_ordinal: Option<i64>=sqlx::query_scalar("SELECT MAX(ordinal) FROM conversation_input_submissions WHERE logical_turn_id=?1 AND card_id=?2")
        .bind(logical_turn_id.as_str()).bind(&carrier.card_id).fetch_one(&mut **tx).await?;
    let ordinal = previous_ordinal
        .and_then(|value| u32::try_from(value).ok())
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            CalmError::Conflict("logical Turn has no Initial or its submission order is exhausted")
        })?;
    let submission_id = InputSubmissionId::parse(&new_id())
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    let original_digest = insert_submission_tx(
        tx,
        &carrier.card_id,
        logical_turn_id,
        &submission_id,
        ordinal,
        &plan,
        &entries,
    )
    .await?;
    carrier.checkpoint.pending.remove(position);
    carrier
        .checkpoint
        .active
        .as_mut()
        .expect("validated active Turn")
        .submission_ids
        .push(submission_id.clone());
    save_input_carrier_tx(tx, carrier).await?;
    Ok(ClaimedSubmission {
        submission_id,
        logical_turn_id: logical_turn_id.clone(),
        ordinal,
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
