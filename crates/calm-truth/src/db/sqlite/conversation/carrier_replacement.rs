//! One-card replacement only. The caller owns authorization, lifecycle exclusion,
//! revision policy, root publication, commit and runtime publication.
use super::{CurrentInputCarrier, RuntimeExecutionFence, current_input_carrier_tx};
use crate::error::{CalmError, Result};
use crate::planner_binding::PlannerBinding;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use calm_types::worker::*;
use sqlx::{Sqlite, Transaction};

/// Caller-owned protected facts (native idle/detach, ingress, asks, dispatch and
/// MCP calls). Implementations must retain their owner guards until publication.
/// This callback performs only transaction-local checks, never provider I/O.
#[async_trait::async_trait]
pub trait CarrierReplacementGuard: Send + Sync {
    async fn recheck_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<()>;
}

/// Required caller revision choice; Compare validates caller-owned CAS in this transaction.
pub enum CallerRevision<'a> {
    Unversioned,
    Compare(&'a dyn CarrierReplacementGuard),
}

pub struct ExpectedCarrier<'a> {
    pub card_id: String,
    pub track_id: String,
    pub worker_session_id: String,
    pub original_snapshot: String,
    pub execution: RuntimeExecutionFence,
    pub binding: PlannerBinding,
    pub accepted_prefix: i64,
    pub ingress: super::CarrierIngressRevision,
    pub owner: &'a dyn CarrierReplacementGuard,
    pub revision: CallerRevision<'a>,
}

pub struct CarrierReplacement {
    pub card_id: String,
    pub track_id: String,
    pub old_worker: WorkerSession,
    pub new_worker: WorkerSession,
    pub old_binding: PlannerBinding,
    pub new_binding: PlannerBinding,
    pub watermark: i64,
    /// Last existing presentation sequence, never a rewritten history cursor.
    pub history_boundary: i64,
    /// The caller must explicitly keep/repoint this link in its transaction.
    pub root_session_id: Option<String>,
    pub revoked_card_tokens: u64,
    pub revoked_session_mirrors: u64,
}

pub async fn replace_idle_carrier_tx(
    tx: &mut Transaction<'_, Sqlite>,
    expected: ExpectedCarrier<'_>,
    next: PlannerBinding,
) -> Result<CarrierReplacement> {
    let carrier =
        current_input_carrier_tx(tx, &expected.card_id, &expected.worker_session_id).await?;
    let frame: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
    if carrier.original_snapshot() != expected.original_snapshot
        || RuntimeExecutionFence::from_carrier(&carrier) != expected.execution
        || frame.header.push_watermark != expected.accepted_prefix
    {
        return Err(CalmError::Conflict(
            "replacement carrier snapshot or prefix changed",
        ));
    }
    let shape = super::super::card_execution_shape_tx(tx, &expected.card_id).await?;
    let binding = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
        .ok_or(CalmError::Conflict("replacement binding unavailable"))?;
    let old =
        super::super::session_get_tx(tx, &WorkerSessionId(expected.worker_session_id.clone()))
            .await?
            .ok_or(CalmError::Conflict("replacement worker unavailable"))?;
    if binding.profile == calm_types::harness::HarnessProfile::PlainChat
        || binding != expected.binding
        || !binding.matches_session_contract(&old)
        || old.track_id.as_str() != expected.track_id
        || !matches!(
            old.state,
            WorkerSessionState::Idle | WorkerSessionState::Running
        )
        || old.thread_id != frame.header.last_thread_id
        || old.active_turn_id.is_some()
        || !frame.idle_for_lifecycle()
        || !frame.canonical_inputs.referenced_submissions().is_empty()
    {
        return Err(CalmError::Conflict(
            "replacement requires the unchanged idle carrier",
        ));
    }
    if super::super::acp_submission_unresolved_tx(tx, &expected.worker_session_id).await? {
        return Err(CalmError::Conflict(
            "replacement has an unresolved native submission",
        ));
    }
    if super::carrier_ingress_revision_tx(
        tx,
        &expected.card_id,
        &expected.track_id,
        &expected.worker_session_id,
    )
    .await?
        != expected.ingress
    {
        return Err(CalmError::Conflict("replacement ingress changed"));
    }
    expected.owner.recheck_tx(tx, &carrier).await?;
    if let CallerRevision::Compare(revision) = expected.revision {
        revision.recheck_tx(tx, &carrier).await?;
    }

    // Hooks may inspect caller-owned SQL state but cannot invalidate the facts
    // already compared above (including an in-transaction concurrent save).
    if super::carrier_ingress_revision_tx(
        tx,
        &expected.card_id,
        &expected.track_id,
        &expected.worker_session_id,
    )
    .await?
        != expected.ingress
    {
        return Err(CalmError::Conflict(
            "replacement ingress changed during owner check",
        ));
    }
    let root_session_id = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
        .bind(&expected.track_id)
        .fetch_one(&mut **tx)
        .await?;
    let history_boundary: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence),0) FROM conversation_presentations WHERE card_id=?1",
    )
    .bind(&expected.card_id)
    .fetch_one(&mut **tx)
    .await?;
    let (role, payload) = next.conversation_shape(shape.payload)?;
    let now = crate::model::now_ms();
    // Owner performs the card-scoped sweep before supersede can clear a subset.
    let (revoked_card_tokens, revoked_session_mirrors) =
        super::super::session_row::revoke_card_credentials_tx(tx, &expected.card_id).await?;
    super::super::session_supersede_active_tx(tx, &expected.worker_session_id, now)
        .await
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let fresh = WorkerSession {
        id: WorkerSessionId(crate::model::new_id()),
        track_id: old.track_id.clone(),
        card_id: old.card_id.clone(),
        provider: WorkerProviderKind::from(&next.provider),
        mode: SessionMode::Resumable,
        contract: next.contract(),
        state: WorkerSessionState::Idle,
        parent_session_id: None,
        requester_session_id: None,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        handle_state_json: Some(serde_json::to_value(CanonicalHarnessSnapshot::dormant(
            expected.accepted_prefix,
        ))?),
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: now,
        updated_at_ms: now,
        completed_at_ms: None,
    };
    let fresh = super::super::session_insert_tx(tx, fresh).await?;
    let changed = sqlx::query("UPDATE cards SET session_id=?1,role=?2,payload=?3 WHERE id=?4 AND session_id=?5 AND role=?6")
        .bind(fresh.id.as_str()).bind(role.as_db_str()).bind(serde_json::to_string(&payload)?)
        .bind(&expected.card_id).bind(&expected.worker_session_id).bind(shape.role.as_db_str())
        .execute(&mut **tx).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict("replacement pointer changed"));
    }
    Ok(CarrierReplacement {
        card_id: expected.card_id,
        track_id: expected.track_id,
        old_worker: old,
        new_worker: fresh,
        old_binding: binding,
        new_binding: next,
        watermark: expected.accepted_prefix,
        history_boundary,
        root_session_id,
        revoked_card_tokens,
        revoked_session_mirrors,
    })
}

impl CarrierReplacement {
    /// Explicit root choice by the caller. Historical parent/requester links are
    /// factual fields of old_worker and are never repointed by this operation.
    pub async fn repoint_root_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        current_input_carrier_tx(tx, &self.card_id, self.new_worker.id.as_str()).await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
                .bind(&self.track_id)
                .fetch_one(&mut **tx)
                .await?;
        if current != self.root_session_id {
            return Err(CalmError::Conflict("replacement root changed"));
        }
        super::super::session_mark_track_root_tx(tx, &self.new_worker.track_id, &self.new_worker.id)
            .await
    }
}
