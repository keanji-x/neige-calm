//! One leased domain workflow. Recovery never acquires the original live capability.
use super::handoff_coordinator::{
    self as coordinator, Claim, Claims, CommittedIdentity, CoordinatorInput, HANDOFF_COORDINATOR,
    Progress, Resources,
};
use super::handoff_coordinator_sql as journal;
use super::handoff_cutover::{self, CutoverResult, CutoverScope, conflict};
use super::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};
use crate::error::{CalmError, Result};
use crate::harness::handoff_pause::{PauseQuery, PauseRequest, PauseState};
use crate::harness::{HarnessRegistry, PlannerHarness};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

pub(crate) struct HandoffCoordinatorAdapter {
    pub registry: HarnessRegistry,
    pub cache: crate::card_role_cache::CardRoleCache,
    pub start: Arc<PlannerHarnessStartAdapter>,
}
const PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::AwaitingRetry,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];
pub(super) fn continuation() -> AppServerInteractKind {
    AppServerInteractKind::RegisterPending { entry_id: None }
}

impl HandoffCoordinatorAdapter {
    async fn reload_output(&self, ctx: &SpawnCtx, op: &Operation) -> Result<TxOutput> {
        // Serialize with a commit whose answer was lost. A before-image from a concurrent
        // ordinary SELECT must never authorize compensation of committed identity.
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let stored: String = sqlx::query_scalar(
            "SELECT tx_output_json FROM operations WHERE id=?1 AND kind=?2 AND payload_hash=?3",
        )
        .bind(&op.id)
        .bind(&op.kind)
        .bind(&op.payload_hash)
        .fetch_one(&mut *tx)
        .await?;
        let output = serde_json::from_str(&stored)?;
        tx.rollback().await?;
        Ok(output)
    }
    fn owner(&self, request: &PauseRequest) -> Result<PlannerHarness> {
        let (_, session, _) = request.identity();
        self.registry.get(&session.to_string()).ok_or_else(|| {
            CalmError::Conflict(
                "Original handoff owner unavailable; explicit recovery required".into(),
            )
        })
    }
    pub(super) async fn checkpoint(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        progress: &Progress,
    ) -> Result<()> {
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let mut next = output.clone();
        next.result = serde_json::to_value(progress)?;
        super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
            .await?;
        if progress.terminal() {
            let released:Option<bool>=sqlx::query_scalar("UPDATE planner_handoff_coordinators SET active=0 WHERE operation_id=?1 AND active=1 RETURNING active")
                .bind(&op.id).fetch_optional(&mut *tx).await?;
            if released != Some(false) {
                return Err(conflict());
            }
        }
        tx.commit().await?;
        *output = next;
        Ok(())
    }
    async fn pause(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        claims: &mut Claims,
        input: &CoordinatorInput,
        source: bool,
    ) -> Result<()> {
        if !coordinator::live_admission(op)
            || journal::cancelled(&ctx.operation_repo.sqlite_pool(), op).await?
        {
            return Err(conflict());
        }
        let request = if source { &input.source } else { &input.target };
        let owner = self.owner(request)?;
        if source {
            claims.source = Claim::Intent;
        } else {
            claims.target = Claim::Intent;
        }
        // Generation and lease are revalidated in the same transaction as the effect intent.
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        journal::validate_generation(&mut tx, &input.request).await?;
        let mut next = output.clone();
        next.result = serde_json::to_value(Progress::Pausing {
            claims: claims.clone(),
        })?;
        super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
            .await?;
        tx.commit().await?;
        *output = next;
        self.seam(
            op,
            if source {
                "SourceIntent"
            } else {
                "TargetIntent"
            },
        )
        .await;
        let answer = owner.attempt_handoff_pause(request.clone()).await;
        match answer {
            crate::harness::handoff_pause::PauseAttempt::Recorded(receipt)
                if receipt.request() != request =>
            {
                return Err(conflict());
            }
            crate::harness::handoff_pause::PauseAttempt::Refused(error) => {
                if source {
                    claims.source = Claim::Untouched;
                } else {
                    claims.target = Claim::Untouched;
                }
                self.checkpoint(
                    ctx,
                    op,
                    output,
                    &Progress::Pausing {
                        claims: claims.clone(),
                    },
                )
                .await?;
                return Err(error);
            }
            crate::harness::handoff_pause::PauseAttempt::Unknown(error) => {
                tracing::warn!(operation_id=%op.id, %error, "Reconciling original pause answer")
            }
            _ => {}
        }
        let PauseQuery::Recorded(PauseState::Paused { receipt }) =
            crate::db::handoff_pause::query_handoff_pause_pool(&pool, request.clone()).await?
        else {
            return Err(conflict());
        };
        if receipt.request() != request {
            return Err(conflict());
        }
        if source {
            claims.source = Claim::Paused { receipt };
        } else {
            claims.target = Claim::Paused { receipt };
        }
        self.checkpoint(
            ctx,
            op,
            output,
            &Progress::Pausing {
                claims: claims.clone(),
            },
        )
        .await?;
        self.seam(op, if source { "SourcePaused" } else { "BothPaused" })
            .await;
        Ok(())
    }
    async fn seam(&self, _op: &Operation, _stage: &str) {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "handoff-coordinator-checkpoint",
            &format!("{}:{}", _op.id, _stage),
        )
        .await;
    }
    async fn commit_identity(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        input: &CoordinatorInput,
        claims: Claims,
    ) -> Result<()> {
        let Claim::Paused {
            receipt: source_pause,
        } = claims.source
        else {
            return Err(conflict());
        };
        let Claim::Paused {
            receipt: target_pause,
        } = claims.target
        else {
            return Err(conflict());
        };
        let plan = handoff_cutover::precheck(
            &ctx.operation_repo.sqlite_pool(),
            CutoverScope {
                track_id: input.request.track_id.clone(),
                source_card_id: input.request.source_card_id.clone(),
                source_session_id: input.request.source_session_id.clone(),
                target_card_id: input.request.target_card_id.clone(),
                target_session_id: input.request.target_session_id.clone(),
                source_pause: source_pause.clone(),
                target_pause: target_pause.clone(),
                plan_id: input.plan_id.clone(),
                new_session_id: input.new_session_id.clone(),
                user_audit_id: input.request.user_audit_id.clone(),
            },
        )
        .await?;
        self.seam(op, "BeforeCommit").await;
        if !coordinator::live_admission(op) {
            return Err(conflict());
        }
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        journal::validate_generation(&mut tx, &input.request).await?;
        if sqlx::query_scalar::<_, bool>(
            "SELECT cancel_requested FROM planner_handoff_coordinators WHERE operation_id=?1",
        )
        .bind(&op.id)
        .fetch_one(&mut *tx)
        .await?
        {
            return Err(conflict());
        }
        let CutoverResult::CutoverCommittedPendingActivation { push_watermark, .. } =
            super::handoff_cutover_sql::cutover(&mut tx, &plan, &op.id).await?;
        let identity = CommittedIdentity {
            origin: super::handoff_coordinator::IdentityOrigin::Workflow,
            configuration: plan.configuration.clone(),
            source_pause,
            target_pause,
            push_watermark,
        };
        let mut next = output.clone();
        next.result = serde_json::to_value(Progress::Committed {
            identity,
            resources: Resources::Unprepared,
        })?;
        super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
            .await?;
        tx.commit().await?;
        *output = next;
        handoff_cutover::publish_cache(&pool, &self.cache, &plan).await?;
        self.seam(op, "Committed").await;
        Ok(())
    }
    async fn resolve(&self, ctx: &SpawnCtx, request: &PauseRequest) -> Result<PauseQuery> {
        let fact = crate::db::handoff_pause::query_handoff_pause_pool(
            &ctx.operation_repo.sqlite_pool(),
            request.clone(),
        )
        .await?;
        if matches!(fact, PauseQuery::Unknown) {
            self.owner(request)?
                .resolve_original_handoff_pause(request.clone())
                .await
        } else {
            Ok(fact)
        }
    }
    async fn compensate(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        input: &CoordinatorInput,
        mut claims: Claims,
        reason: String,
    ) -> Result<AppServerInteractOutcome> {
        self.checkpoint(
            ctx,
            op,
            output,
            &Progress::Compensating {
                claims: claims.clone(),
                reason: reason.clone(),
            },
        )
        .await?;
        for source in [false, true] {
            let claim = if source {
                &claims.source
            } else {
                &claims.target
            };
            if matches!(claim, Claim::Untouched | Claim::Released { .. }) {
                continue;
            }
            let request = if source { &input.source } else { &input.target };
            let resolved = match self.resolve(ctx, request).await {
                Ok(PauseQuery::Unknown) if matches!(claim, Claim::Intent) => Some(Claim::Untouched),
                Ok(PauseQuery::Recorded(PauseState::Released { receipt, outcome })) => {
                    Some(Claim::Released { receipt, outcome })
                }
                Ok(PauseQuery::Recorded(
                    PauseState::Paused { receipt }
                    | PauseState::FaultedPaused { receipt, .. }
                    | PauseState::RecoveryHealthUnconfirmed { receipt },
                )) => {
                    if let Claim::Paused { receipt: expected } = claim
                        && expected != &receipt
                    {
                        return Err(conflict());
                    }
                    let recorded = Claim::Paused {
                        receipt: receipt.clone(),
                    };
                    if source {
                        claims.source = recorded;
                    } else {
                        claims.target = recorded;
                    }
                    self.checkpoint(
                        ctx,
                        op,
                        output,
                        &Progress::Compensating {
                            claims: claims.clone(),
                            reason: reason.clone(),
                        },
                    )
                    .await?;
                    match self.owner(request) {
                        Ok(owner) => owner
                            .resume_from_handoff(receipt.clone())
                            .await
                            .ok()
                            .map(|outcome| Claim::Released { receipt, outcome }),
                        Err(_) => None,
                    }
                }
                _ => None,
            };
            if let Some(resolved) = resolved {
                if source {
                    claims.source = resolved;
                } else {
                    claims.target = resolved;
                }
                self.checkpoint(
                    ctx,
                    op,
                    output,
                    &Progress::Compensating {
                        claims: claims.clone(),
                        reason: reason.clone(),
                    },
                )
                .await?;
            }
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "handoff-coordinator-before-settlement",
            &input.request.source_session_id,
        )
        .await;
        if let Ok(settled) = super::handoff_coordinator::SettledClaims::try_from(claims.clone()) {
            self.checkpoint(ctx, op, output, &Progress::Finished { claims: settled })
                .await?;
            Ok(AppServerInteractOutcome::NotApplicable)
        } else {
            self.checkpoint(
                ctx,
                op,
                output,
                &Progress::Compensating {
                    claims,
                    reason: format!("Exact compensation requires recovery: {reason}"),
                },
            )
            .await?;
            Ok(AppServerInteractOutcome::AwaitingRetry)
        }
    }
    async fn run(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
    ) -> Result<AppServerInteractOutcome> {
        let input: CoordinatorInput = serde_json::from_value(op.payload.clone())?;
        if input.version != 2 {
            return Err(conflict());
        }
        let state: Progress = serde_json::from_value(output.result.clone())?;
        if state.terminal() {
            return Ok(AppServerInteractOutcome::NotApplicable);
        }
        if matches!(state, Progress::Reserved) && coordinator::live_admission(op) {
            let result = async {
                let mut claims = Claims::empty();
                self.pause(ctx, op, output, &mut claims, &input, true)
                    .await?;
                self.pause(ctx, op, output, &mut claims, &input, false)
                    .await?;
                self.commit_identity(ctx, op, output, &input, claims).await
            }
            .await;
            // Read the original row before interpreting any failed commit answer.
            if let Err(error) = result {
                *output = self.reload_output(ctx, op).await?;
                match serde_json::from_value::<Progress>(output.result.clone())? {
                    Progress::Pausing { claims } => {
                        return self
                            .compensate(ctx, op, output, &input, claims, error.to_string())
                            .await;
                    }
                    Progress::Reserved => {
                        return self
                            .compensate(ctx, op, output, &input, Claims::empty(), error.to_string())
                            .await;
                    }
                    Progress::Committed { .. } | Progress::Activated { .. } => {}
                    _ => return Err(error),
                }
            }
        }
        match serde_json::from_value::<Progress>(output.result.clone())? {
            Progress::Reserved => {
                self.compensate(
                    ctx,
                    op,
                    output,
                    &input,
                    Claims::empty(),
                    "Interrupted original begin; no new pause permitted".into(),
                )
                .await
            }
            Progress::Pausing { claims } => {
                self.compensate(
                    ctx,
                    op,
                    output,
                    &input,
                    claims,
                    "Interrupted preparation; original claims only".into(),
                )
                .await
            }
            Progress::Compensating { claims, reason } => {
                self.compensate(ctx, op, output, &input, claims, reason)
                    .await
            }
            Progress::Committed {
                identity,
                resources,
            } => {
                self.forward_resources(ctx, op, output, &input, identity, resources)
                    .await
            }
            _ => Ok(AppServerInteractOutcome::NotApplicable),
        }
    }
}
#[async_trait]
impl ProviderAdapter for HandoffCoordinatorAdapter {
    fn kind(&self) -> &'static str {
        HANDOFF_COORDINATOR
    }
    fn phases(&self) -> &'static [PhaseTag] {
        PHASES
    }
    fn app_server_interact_kind(
        &self,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(continuation())
    }
    fn interrupted_interaction_retry(
        &self,
        op: &Operation,
    ) -> Option<super::ExplicitRetryContinuation> {
        if !matches!(op.phase, super::Phase::Stuck { .. }) {
            return None;
        }
        match &op.tx_output {
            None => {
                serde_json::from_value::<CoordinatorInput>(op.payload.clone()).ok()?;
                Some(super::ExplicitRetryContinuation::Prepare)
            }
            Some(output) => {
                let p: Progress = serde_json::from_value(output.result.clone()).ok()?;
                if p.terminal() {
                    return None;
                }
                Some(super::ExplicitRetryContinuation::Interact(continuation()))
            }
        }
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        Err(CalmError::Conflict(
            "Handoff requires original internal audited admission".into(),
        ))
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let input: CoordinatorInput = serde_json::from_value(input.clone())?;
        journal::validate_reservation(tx, op, &input).await?;
        journal::output(&input)
    }
    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        match self.run(ctx, op, output).await {
            Ok(result) => Ok(result),
            Err(error) => {
                tracing::warn!(operation_id=%op.id, %error, "Handoff requires explicit original-generation recovery");
                *output = self.reload_output(ctx, op).await?;
                let p: Progress = serde_json::from_value(output.result.clone())?;
                // Storage errors never rewrite evidence or downgrade committed terminals.
                if p.terminal() {
                    Ok(AppServerInteractOutcome::NotApplicable)
                } else {
                    let pool = ctx.operation_repo.sqlite_pool();
                    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
                    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                    sqlx::query("UPDATE operations SET last_error=?1 WHERE id=?2")
                        .bind(error.to_string())
                        .bind(&op.id)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    Ok(AppServerInteractOutcome::AwaitingRetry)
                }
            }
        }
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Err(conflict())
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        Err(conflict())
    }
}
