//! Sequential composition; every intent checkpoints under OperationRuntime's exact lease.
use super::handoff_coordinator::{CoordinatorInput, HANDOFF_COORDINATOR, Progress, Stage};
use super::handoff_coordinator_sql as journal;
use super::handoff_cutover::{self, CutoverResult, CutoverScope, conflict};
use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};
use crate::error::{CalmError, Result};
use crate::harness::handoff_pause::{
    PauseQuery, PauseReceipt, PauseRequest, PauseState, ReleaseOutcome,
};
use crate::harness::{HarnessRegistry, PlannerHarness};
use async_trait::async_trait;
use serde_json::Value;

pub(crate) struct HandoffCoordinatorAdapter {
    pub registry: HarnessRegistry,
    pub cache: crate::card_role_cache::CardRoleCache,
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

fn continuation() -> AppServerInteractKind {
    AppServerInteractKind::RegisterPending { entry_id: None }
}

impl HandoffCoordinatorAdapter {
    fn owner(&self, request: &PauseRequest) -> Result<PlannerHarness> {
        let (_, session, _) = request.identity();
        self.registry.get(&session.to_string()).ok_or_else(|| {
            CalmError::Conflict(
                "Original handoff owner unavailable; explicit recovery required".into(),
            )
        })
    }

    async fn checkpoint(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        progress: &Progress,
        terminal: bool,
    ) -> Result<()> {
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        if matches!(progress.stage, Stage::SourceIntent | Stage::TargetIntent) {
            let input: CoordinatorInput = serde_json::from_value(op.payload.clone())?;
            journal::validate_generation(&mut tx, &input.request).await?;
        }
        let mut next = output.clone();
        next.result = serde_json::to_value(progress)?;
        super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
            .await?;
        if terminal {
            sqlx::query("UPDATE planner_handoff_coordinators SET active=0 WHERE operation_id=?1")
                .bind(&op.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        *output = next;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "handoff-coordinator-checkpoint",
            &format!("{}:{:?}", op.id, progress.stage),
        )
        .await;
        Ok(())
    }

    async fn pause(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        p: &mut Progress,
        input: &CoordinatorInput,
        source: bool,
    ) -> Result<()> {
        if journal::cancelled(&ctx.operation_repo.sqlite_pool(), op).await? {
            return Err(conflict());
        }
        let request = if source { &input.source } else { &input.target };
        let owner = self.owner(request)?;
        // No await with provider effects occurs until this original claim intent is durable.
        if source {
            p.source_attempted = true;
            p.stage = Stage::SourceIntent;
        } else {
            p.target_attempted = true;
            p.stage = Stage::TargetIntent;
        }
        self.checkpoint(ctx, op, output, p, false).await?;
        let answer = owner.attempt_handoff_pause(request.clone()).await;
        let (refused, failure) = match &answer {
            crate::harness::handoff_pause::PauseAttempt::Recorded(receipt) => {
                if receipt.request() != request {
                    return Err(conflict());
                }
                (false, None)
            }
            crate::harness::handoff_pause::PauseAttempt::Refused(error) => {
                (true, Some(error.to_string()))
            }
            crate::harness::handoff_pause::PauseAttempt::Unknown(error) => {
                (false, Some(error.to_string()))
            }
        };
        if refused {
            if source {
                p.source_attempted = false;
            } else {
                p.target_attempted = false;
            }
            self.checkpoint(ctx, op, output, p, false).await?;
            return Err(CalmError::Conflict(
                failure.unwrap_or_else(|| "Pause refused".into()),
            ));
        }
        // Always resolve the original SQL fact, including a missing reply or faulted receipt.
        let fact = crate::db::handoff_pause::query_handoff_pause_pool(
            &ctx.operation_repo.sqlite_pool(),
            request.clone(),
        )
        .await?;
        let PauseQuery::Recorded(PauseState::Paused { receipt }) = fact else {
            return Err(CalmError::Conflict(
                failure.unwrap_or_else(|| "Pause is not healthy".into()),
            ));
        };
        if source {
            p.source_receipt = Some(receipt);
            p.stage = Stage::SourcePaused;
        } else {
            p.target_receipt = Some(receipt);
            p.stage = Stage::BothPaused;
        }
        self.checkpoint(ctx, op, output, p, false).await
    }

    async fn prepare(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        p: &mut Progress,
        input: &CoordinatorInput,
    ) -> Result<()> {
        self.pause(ctx, op, output, p, input, true).await?;
        self.pause(ctx, op, output, p, input, false).await?;
        if journal::cancelled(&ctx.operation_repo.sqlite_pool(), op).await? {
            return Err(conflict());
        }
        let plan = handoff_cutover::precheck(
            &ctx.operation_repo.sqlite_pool(),
            CutoverScope {
                track_id: input.request.track_id.clone(),
                source_card_id: input.request.source_card_id.clone(),
                source_session_id: input.request.source_session_id.clone(),
                target_card_id: input.request.target_card_id.clone(),
                target_session_id: input.request.target_session_id.clone(),
                source_pause: p.source_receipt.clone().ok_or_else(conflict)?,
                target_pause: p.target_receipt.clone().ok_or_else(conflict)?,
                plan_id: input.plan_id.clone(),
                new_session_id: input.new_session_id.clone(),
                user_audit_id: input.request.user_audit_id.clone(),
            },
        )
        .await?;
        p.plan = Some(plan.clone());
        p.stage = Stage::PlanRecorded;
        self.checkpoint(ctx, op, output, p, false).await?;
        if journal::cancelled(&ctx.operation_repo.sqlite_pool(), op).await? {
            return Err(conflict());
        }
        p.stage = Stage::CommitIntent;
        self.checkpoint(ctx, op, output, p, false).await?;
        // Cutover owns final SQL validation; no copied provider/authentication policy here.
        let result = handoff_cutover::commit_recorded(
            &ctx.operation_repo.sqlite_pool(),
            &self.cache,
            plan,
            op,
        )
        .await?;
        p.committed = Some(result);
        p.stage = Stage::Committed;
        self.checkpoint(ctx, op, output, p, false).await
    }

    async fn resolve_release(&self, ctx: &SpawnCtx, request: &PauseRequest) -> Result<PauseQuery> {
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

    fn record_receipt(p: &mut Progress, source: bool, receipt: &PauseReceipt) -> Result<()> {
        let slot = if source {
            &mut p.source_receipt
        } else {
            &mut p.target_receipt
        };
        if slot.as_ref().is_some_and(|original| original != receipt) {
            return Err(conflict());
        }
        *slot = Some(receipt.clone());
        Ok(())
    }

    async fn compensate(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        p: &mut Progress,
        input: &CoordinatorInput,
    ) -> Result<bool> {
        // A possible committed cutover can NEVER enter this function.
        if p.committed.is_some() || p.stage == Stage::CommitIntent {
            return Err(conflict());
        }
        p.stage = Stage::Compensating;
        self.checkpoint(ctx, op, output, p, false).await?;
        // Resolve both independently; a failed target release must not orphan a source claim.
        let mut errors = vec![];
        for source in [false, true] {
            let (attempted, done, request) = if source {
                (
                    p.source_attempted,
                    p.source_release.is_some(),
                    &input.source,
                )
            } else {
                (
                    p.target_attempted,
                    p.target_release.is_some(),
                    &input.target,
                )
            };
            if !attempted || done {
                continue;
            }
            let result = match self.resolve_release(ctx, request).await {
                Ok(PauseQuery::Recorded(PauseState::Released { receipt, outcome })) => {
                    Self::record_receipt(p, source, &receipt)?;
                    Ok(Some(outcome))
                }
                Ok(PauseQuery::Recorded(
                    PauseState::Paused { receipt }
                    | PauseState::FaultedPaused { receipt, .. }
                    | PauseState::RecoveryHealthUnconfirmed { receipt },
                )) => {
                    // Persist the real queried receipt before releasing an uncertain pause.
                    Self::record_receipt(p, source, &receipt)?;
                    self.checkpoint(ctx, op, output, p, false).await?;
                    match self.owner(request) {
                        Ok(owner) => owner.resume_from_handoff(receipt).await.map(Some),
                        Err(error) => Err(error),
                    }
                }
                Ok(PauseQuery::Unknown)
                    if (if source {
                        &p.source_receipt
                    } else {
                        &p.target_receipt
                    })
                    .is_none() =>
                {
                    Ok(None)
                }
                Ok(_) => Err(CalmError::Conflict(
                    "Original pause result unresolved; keep reservation and claim for recovery"
                        .into(),
                )),
                Err(error) => Err(error),
            };
            match result {
                Ok(outcome) => {
                    if source {
                        if outcome.is_none() {
                            p.source_attempted = false;
                        }
                        p.source_release = outcome;
                    } else {
                        if outcome.is_none() {
                            p.target_attempted = false;
                        }
                        p.target_release = outcome;
                    }
                    self.checkpoint(ctx, op, output, p, false).await?;
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        let healthy = errors.is_empty()
            && [
                (p.source_attempted, &p.source_release),
                (p.target_attempted, &p.target_release),
            ]
            .iter()
            .all(|(attempted, outcome)| {
                !attempted || matches!(outcome, Some(ReleaseOutcome::HealthyReleased))
            });
        let resolved = errors.is_empty()
            && [
                (p.source_attempted, &p.source_release),
                (p.target_attempted, &p.target_release),
            ]
            .iter()
            .all(|(attempted, outcome)| !attempted || outcome.is_some());
        if healthy {
            p.stage = Stage::Finished;
        } else {
            p.reason = Some(format!(
                "Exact compensation requires recovery: {}",
                errors.join("; ")
            ));
            if resolved {
                p.stage = Stage::ReleasedNeedsRecovery;
            }
        }
        // A known nonhealthy release frees the claim, never proves provider health. Only
        // unresolved claims/unknown storage retain the in-flight reservation and retry.
        self.checkpoint(ctx, op, output, p, resolved).await?;
        Ok(resolved)
    }

    async fn forward(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        p: &mut Progress,
    ) -> Result<()> {
        let Some(CutoverResult::CutoverCommittedPendingActivation { operation_id, .. }) =
            &p.committed
        else {
            return Err(conflict());
        };
        super::handoff_activation::enqueue(ctx.operation_repo.as_ref(), operation_id.clone())
            .await?;
        p.reason = None;
        p.stage = Stage::Committed;
        self.checkpoint(ctx, op, output, p, false).await
    }

    async fn run(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
    ) -> Result<AppServerInteractOutcome> {
        let input: CoordinatorInput = serde_json::from_value(op.payload.clone())?;
        let mut p: Progress = serde_json::from_value(output.result.clone())?;
        let fresh = p.stage == Stage::Reserved;
        let prepared = if fresh {
            self.prepare(ctx, op, output, &mut p, &input).await
        } else {
            Ok(())
        };
        if let Err(error) = prepared {
            // A checkpoint may have committed but lost its answer. Never overwrite its progress
            // with the caller's staged before-image.
            let row = ctx
                .operation_repo
                .get_operation(&op.id)
                .await?
                .ok_or_else(conflict)?;
            *output = row.tx_output.ok_or_else(conflict)?;
            p = serde_json::from_value(output.result.clone())?;
            p.reason = Some(error.to_string());
        }
        if p.stage == Stage::CommitIntent {
            // Query the original exact key first, even when the commit call itself errored.
            match handoff_cutover::query_recorded(
                &ctx.operation_repo.sqlite_pool(),
                p.plan.as_ref().ok_or_else(conflict)?,
            )
            .await?
            {
                Some(result) => {
                    p.committed = Some(result);
                    p.stage = Stage::Committed;
                    self.checkpoint(ctx, op, output, &p, false).await?;
                }
                None => {
                    // The serialized original query proved the atomic cutover absent.
                    p.stage = Stage::PlanRecorded;
                    self.checkpoint(ctx, op, output, &p, false).await?;
                }
            }
        }
        if p.committed.is_some() {
            return match self.forward(ctx, op, output, &mut p).await {
                Ok(()) => Ok(AppServerInteractOutcome::NotApplicable),
                Err(error) => {
                    p.reason = Some(format!("Forward activation requires recovery: {error}"));
                    self.checkpoint(ctx, op, output, &p, false).await?;
                    Ok(AppServerInteractOutcome::AwaitingRetry)
                }
            };
        }
        if matches!(p.stage, Stage::Finished | Stage::ReleasedNeedsRecovery) {
            return Ok(AppServerInteractOutcome::NotApplicable);
        }
        if !fresh && p.reason.is_none() {
            p.reason = Some("Interrupted preparation; reconcile original claims only".into());
        }
        if self.compensate(ctx, op, output, &mut p, &input).await? {
            Ok(AppServerInteractOutcome::NotApplicable)
        } else {
            Ok(AppServerInteractOutcome::AwaitingRetry)
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
                serde_json::from_value::<Progress>(output.result.clone()).ok()?;
                Some(super::ExplicitRetryContinuation::Interact(continuation()))
            }
        }
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        // Admission is the typed transactional service, never generic JSON submit.
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
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                // Retain durable progress; errors may not invoke the generic backward saga.
                let row = ctx
                    .operation_repo
                    .get_operation(&op.id)
                    .await?
                    .ok_or_else(conflict)?;
                *output = row.tx_output.ok_or_else(conflict)?;
                let mut p: Progress = serde_json::from_value(output.result.clone())?;
                p.reason = Some(format!("Handoff needs explicit recovery: {error}"));
                self.checkpoint(ctx, op, output, &p, false).await?;
                Ok(AppServerInteractOutcome::AwaitingRetry)
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
        Err(CalmError::Conflict(
            "Coordinator retains original journal; explicit recovery only".into(),
        ))
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
