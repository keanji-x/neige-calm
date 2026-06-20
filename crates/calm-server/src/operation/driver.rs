use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, broadcast};

use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::event::EventBus;
use crate::model::{TaskStatus, now_ms};
use crate::proc_identity::signal_process_group;
use crate::session_projection_repo::{AgentProvider, WorkerSessionState};
use crate::terminal_sweeper::{
    WaitForPidExit, reap_terminal_artifacts_with_renderer, wait_for_pid_exit,
};

use super::workspace_lease::reclaim_dead_workspace_leases_on_boot;
use super::{
    AppServerInteractOutcome, CompensationStateVersioned, Operation, OperationId, OperationKey,
    OperationRepo, OperationResult, ParkedClaimMode, ParkedCompletion, ParkedOutcome,
    ParkedRecovery, Phase, PhaseTag, ProviderAdapter, RecoveryItem, RecoveryMode, RecoveryPlan,
    SpawnArtifacts, SpawnCtx, SpawnOutcome, TxOutput, complete_parked_tx,
    idempotency_payload_conflict, operation_result_from, parked_artifacts_alive, required_output,
};

/// Completion fan-out uses a broadcast channel rather than a oneshot map.
/// That lets `wait()` first check the durable row, then subscribe without
/// losing a completion that raced just before the waiter arrived.
#[derive(Clone)]
pub struct OperationCompletionBus {
    tx: broadcast::Sender<OperationResult>,
}

impl OperationCompletionBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(128);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OperationResult> {
        self.tx.subscribe()
    }

    pub fn complete(&self, result: OperationResult) {
        let _ = self.tx.send(result);
    }
}

impl Default for OperationCompletionBus {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OperationRuntime {
    repo: Arc<dyn OperationRepo>,
    kinds: HashMap<&'static str, Arc<dyn ProviderAdapter>>,
    completion: OperationCompletionBus,
    events: EventBus,
    spawn_ctx: SpawnCtx,
    // PR2: replace with a singleton background driver loop per design §B.3.
    drive_mutex: Mutex<()>,
}

impl OperationRuntime {
    pub async fn new(
        repo: Arc<dyn OperationRepo>,
        kinds: Vec<Arc<dyn ProviderAdapter>>,
        events: EventBus,
        completion: OperationCompletionBus,
        spawn_ctx: SpawnCtx,
    ) -> Result<Self> {
        repo.assert_sqlite_version().await?;
        Ok(Self::new_unchecked(
            repo, kinds, events, completion, spawn_ctx,
        ))
    }

    pub fn new_unchecked(
        repo: Arc<dyn OperationRepo>,
        kinds: Vec<Arc<dyn ProviderAdapter>>,
        events: EventBus,
        completion: OperationCompletionBus,
        spawn_ctx: SpawnCtx,
    ) -> Self {
        let kinds = kinds
            .into_iter()
            .map(|adapter| (adapter.kind(), adapter))
            .collect();
        Self {
            repo,
            kinds,
            completion,
            events,
            spawn_ctx,
            drive_mutex: Mutex::new(()),
        }
    }

    pub fn publish_completion(&self, result: OperationResult) {
        self.completion.complete(result);
    }

    pub async fn submit(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        let adapter = self.adapter(kind)?;
        if let Some(existing) = self.repo.find_by_idempotency_key(kind, &key).await? {
            if existing.payload_hash == key.payload_hash {
                let op_id = existing.id;
                self.drive().await?;
                return Ok(op_id);
            }
            return Err(idempotency_payload_conflict(key.idempotency_key.as_deref()));
        }
        adapter.validate(&payload).await?;
        let op_id = self.repo.insert_operation(kind, key, payload).await?;
        self.drive().await?;
        Ok(op_id)
    }

    pub async fn start(
        &self,
        kind: &str,
        key: OperationKey,
        payload: Value,
    ) -> Result<OperationId> {
        self.submit(kind, key, payload).await
    }

    /// Issue #644 PR-B — look up an operation row by
    /// `(kind, idempotency_key)`. Used by the scheduler's sweep to
    /// correlate a `dispatched`/`running` task row with its worker-spawn
    /// operation (the task-to-operation relation is the idempotency-key
    /// convention, design §2.2; no `spawn_op_id` column exists).
    pub async fn find_by_kind_and_idempotency(
        &self,
        kind: &str,
        idempotency_key: &str,
    ) -> Result<Option<Operation>> {
        self.repo
            .find_by_idempotency_key(
                kind,
                &OperationKey {
                    operation_key: String::new(),
                    idempotency_key: Some(idempotency_key.to_string()),
                    payload_hash: String::new(),
                },
            )
            .await
    }

    pub async fn fail_running_worker_card(&self, card_id: &str) -> Result<()> {
        self.interrupt_running_codex_turn_for_card(card_id).await?;
        if let Some(term) = self.spawn_ctx.repo.terminal_get_by_card(card_id).await? {
            reap_terminal_artifacts_with_renderer(
                Some(self.spawn_ctx.terminal_renderer.as_ref()),
                &term,
            )
            .await;
            if let Some(pid) = term.pid {
                let verdict = wait_for_pid_exit(pid, Duration::from_secs(2)).await;
                match verdict {
                    WaitForPidExit::Exited | WaitForPidExit::InvalidPid => {}
                    WaitForPidExit::StillAliveAfterSigkill | WaitForPidExit::Unsupported => {
                        return Err(CalmError::Internal(format!(
                            "timed-out worker terminal {} did not exit after reap ({verdict:?})",
                            term.id
                        )));
                    }
                }
            }
        } else {
            return Err(CalmError::Internal(format!(
                "timed-out worker card {card_id} has no terminal row"
            )));
        }
        self.spawn_ctx
            .repo
            .session_projection_complete_for_card(card_id, WorkerSessionState::Failed)
            .await?;
        Ok(())
    }

    async fn interrupt_running_codex_turn_for_card(&self, card_id: &str) -> Result<()> {
        let Some(runtime) = self
            .spawn_ctx
            .repo
            .session_projection_active_for_card(&card_id.to_string())
            .await?
        else {
            return Ok(());
        };
        if runtime.agent_provider != Some(AgentProvider::Codex) {
            return Ok(());
        }
        let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref()) else {
            return Ok(());
        };
        let persisted_turn = TxOutput::non_empty_string(runtime.active_turn_id.as_deref());
        let Some(shared_codex_appserver) = self.spawn_ctx.shared_codex_appserver.as_ref() else {
            tracing::warn!(
                runtime_id = %runtime.id,
                card_id = %card_id,
                thread_id = %thread_id,
                "timed-out codex worker has no shared appserver handle; cannot interrupt active turn"
            );
            return Ok(());
        };
        let cached_turn = shared_codex_appserver.active_turn_id_for_thread(&thread_id);
        if let Err(e) = shared_codex_appserver
            .interrupt_active_turn(&thread_id)
            .await
        {
            let turn_id = cached_turn
                .as_deref()
                .or(persisted_turn.as_deref())
                .unwrap_or("");
            tracing::warn!(
                runtime_id = %runtime.id,
                card_id = %card_id,
                thread_id = %thread_id,
                turn_id = %turn_id,
                error = %e,
                "timed-out codex worker turn interrupt failed"
            );
        }
        if cached_turn.is_none()
            && let Some(persisted_turn) = persisted_turn.as_deref()
            && let Err(e) = shared_codex_appserver
                .turn_interrupt(&thread_id, persisted_turn)
                .await
        {
            tracing::warn!(
                runtime_id = %runtime.id,
                card_id = %card_id,
                thread_id = %thread_id,
                turn_id = persisted_turn,
                error = %e,
                "timed-out codex worker persisted-turn interrupt failed"
            );
        }
        Ok(())
    }

    pub async fn wait(&self, op_id: &OperationId) -> Result<OperationResult> {
        if let Some(result) = self.repo.operation_result(op_id).await? {
            return Ok(result);
        }
        let mut rx = self.completion.subscribe();
        loop {
            tokio::select! {
                received = rx.recv() => {
                    match received {
                        Ok(result) if result.op_id == *op_id => return Ok(result),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(CalmError::Internal("operation completion bus closed".into()));
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {
                    if let Some(result) = self.repo.operation_result(op_id).await? {
                        return Ok(result);
                    }
                    self.enforce_parked_deadline(op_id).await?;
                    self.drive().await?;
                }
            }
        }
    }

    pub async fn cancel_parked(&self, op_id: &OperationId, reason: &str) -> Result<bool> {
        let Some(op) = self.repo.claim_parked(op_id).await? else {
            return Ok(false);
        };
        let adapter = self.adapter(&op.kind)?;
        let output = required_output(&op)?.clone();
        let state = adapter
            .plan_compensation(PhaseTag::Parked, reason, &output, &op)
            .await?;
        if self
            .repo
            .set_compensating(&op, &state, &output)
            .await?
            .is_none()
        {
            log_lost_lease(&op, PhaseTag::Compensating);
            return Ok(false);
        }
        self.drive().await?;
        Ok(true)
    }

    pub async fn cancel_inflight_to_compensation(
        &self,
        op_id: &OperationId,
        reason: &str,
    ) -> Result<bool> {
        let Some(op) = self.repo.claim_inflight_for_compensation(op_id).await? else {
            return Ok(false);
        };
        let adapter = self.adapter(&op.kind)?;
        let from_phase = op.phase.tag();
        if matches!(op.phase, Phase::Compensating) {
            if let Some(result) = self.resume_compensation(adapter.as_ref(), op).await? {
                self.completion.complete(result);
            }
            return Ok(true);
        }
        let output = match required_output(&op) {
            Ok(output) => output.clone(),
            Err(_e) if matches!(op.phase, Phase::Pending) => {
                if let Some(result) = self
                    .repo
                    .mark_failed(&op, reason.to_string(), from_phase, Some("internal".into()))
                    .await?
                {
                    self.completion.complete(result);
                } else {
                    log_lost_lease(&op, PhaseTag::Failed);
                    return Ok(false);
                }
                return Ok(true);
            }
            Err(e) => return Err(e),
        };
        let state = adapter
            .plan_compensation(from_phase, reason, &output, &op)
            .await?;
        if self
            .repo
            .set_compensating(&op, &state, &output)
            .await?
            .is_none()
        {
            log_lost_lease(&op, PhaseTag::Compensating);
            return Ok(false);
        }
        self.drive().await?;
        Ok(true)
    }

    pub async fn sweep_parked(&self) -> Result<()> {
        self.sweep_parked_with_claim(ParkedClaimMode::SteadyState)
            .await
    }

    async fn enforce_parked_deadline(&self, op_id: &OperationId) -> Result<()> {
        let Some(op) = self.repo.get_operation(op_id).await? else {
            return Ok(());
        };
        if !matches!(op.phase, Phase::Parked) {
            return Ok(());
        }
        self.apply_parked_sweep(op).await
    }

    pub async fn drive(&self) -> Result<()> {
        self.drive_with_recovery_filter(false).await
    }

    async fn drive_recovery(&self) -> Result<()> {
        self.drive_with_recovery_filter(true).await
    }

    async fn drive_with_recovery_filter(&self, recovery: bool) -> Result<()> {
        let _g = self.drive_mutex.lock().await;
        loop {
            let batch = if recovery {
                self.repo.claim_recovery_drive_batch(32).await?
            } else {
                self.repo.claim_drive_batch(32).await?
            };
            if batch.is_empty() {
                return Ok(());
            }
            for op in batch {
                let from_phase = op.phase.tag();
                let adapter = self.adapter(&op.kind)?;
                if let Err(e) = self.drive_one(adapter, op.clone()).await {
                    if let Some(result) = self
                        .repo
                        .mark_stuck(&op, format!("operation drive failed: {e}"), from_phase)
                        .await?
                    {
                        self.completion.complete(result);
                    } else {
                        log_lost_lease(&op, PhaseTag::Stuck);
                    }
                }
            }
        }
    }

    pub async fn recover_on_boot(&self) -> Result<RecoveryPlan> {
        reclaim_dead_workspace_leases_on_boot(&self.repo.sqlite_pool(), &self.events).await?;
        let rows = self.repo.abandoned_running_operations_on_boot().await?;
        let mut items = Vec::new();
        for op in rows {
            if self.scheduler_owns_dispatched_codex_recovery(&op).await? {
                items.push(RecoveryItem::Skip {
                    op_id: op.id.clone(),
                    reason: "deadline-aware scheduler owns dispatched codex-worker recovery".into(),
                });
                continue;
            }
            let adapter = self.adapter(&op.kind)?;
            items.push(self.plan_recovery_for(adapter.as_ref(), &op).await?);
        }
        Ok(RecoveryPlan { items })
    }

    pub async fn apply_recovery(&self, plan: RecoveryPlan) -> Result<()> {
        for item in plan.items {
            match self.apply_recovery_item(item.clone()).await {
                Ok(()) => {
                    if let Err(e) = self.drive_recovery().await {
                        tracing::error!(
                            error = %e,
                            item = ?item,
                            "operation recovery drive failed; continuing"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        item = ?item,
                        "operation recovery item failed; continuing"
                    );
                }
            }
        }
        self.sweep_parked_for_boot().await?;
        Ok(())
    }

    fn adapter(&self, kind: &str) -> Result<Arc<dyn ProviderAdapter>> {
        self.kinds
            .get(kind)
            .cloned()
            .ok_or_else(|| CalmError::BadRequest(format!("unknown operation kind {kind}")))
    }

    async fn scheduler_owns_dispatched_codex_recovery(&self, op: &Operation) -> Result<bool> {
        if op.kind != "codex-worker" {
            return Ok(false);
        }
        let Some(task_id) = op.idempotency_key.as_deref() else {
            return Ok(false);
        };
        Ok(self
            .spawn_ctx
            .repo
            .task_get(task_id)
            .await?
            .is_some_and(|task| task.status == TaskStatus::Dispatched))
    }

    async fn drive_one(&self, adapter: Arc<dyn ProviderAdapter>, op: Operation) -> Result<()> {
        match op.phase.clone() {
            Phase::Pending => {
                let prepared = self
                    .repo
                    .prepare_tx_and_advance(&op, adapter.as_ref())
                    .await;
                let Some((_next, events)) = (match prepared {
                    Ok(prepared) => prepared,
                    Err(e) => {
                        if let Some((last_error, last_error_class)) = client_failure_parts(&e) {
                            if let Some(result) = self
                                .repo
                                .mark_failed(
                                    &op,
                                    last_error,
                                    PhaseTag::Pending,
                                    Some(last_error_class.to_string()),
                                )
                                .await?
                            {
                                self.completion.complete(result);
                            } else {
                                log_lost_lease(&op, PhaseTag::Failed);
                            }
                            return Ok(());
                        }
                        return Err(e);
                    }
                }) else {
                    log_lost_lease(&op, PhaseTag::TxCommitted);
                    return Ok(());
                };
                for envelope in events {
                    self.events.emit_envelope(envelope);
                }
                Ok(())
            }
            Phase::TxCommitted => {
                if adapter.phases().contains(&PhaseTag::AppServerInteract) {
                    let output = required_output(&op)?;
                    let kind = adapter.app_server_interact_kind(output, &op)?;
                    if self
                        .repo
                        .set_phase(&op, Phase::AppServerInteract { kind })
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::AppServerInteract);
                    }
                    return Ok(());
                }
                if !adapter.phases().contains(&PhaseTag::SpawnStarted) {
                    let output = required_output(&op)?.clone();
                    match adapter
                        .spawn_side_effect(&output, &op, &self.spawn_ctx)
                        .await
                    {
                        Ok(SpawnOutcome::Ready(_handle)) => {
                            if let Some(result) = self.repo.set_phase(&op, Phase::Succeeded).await?
                            {
                                if let Some(result) = operation_result_from(&result)? {
                                    self.completion.complete(result);
                                }
                            } else {
                                log_lost_lease(&op, PhaseTag::Succeeded);
                            }
                        }
                        Ok(SpawnOutcome::Parked { .. }) => {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::TxCommitted,
                                "adapter returned parked from tx_committed spawn branch".into(),
                                output,
                            )
                            .await?;
                        }
                        Err(e) => {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::TxCommitted,
                                e.to_string(),
                                output,
                            )
                            .await?;
                        }
                    }
                    return Ok(());
                }
                if self
                    .repo
                    .set_phase(&op, Phase::SpawnStarted)
                    .await?
                    .is_none()
                {
                    log_lost_lease(&op, PhaseTag::SpawnStarted);
                }
                Ok(())
            }
            Phase::AppServerInteract { .. } => {
                let mut output = required_output(&op)?.clone();
                match adapter
                    .app_server_interact(&mut output, &op, &self.spawn_ctx)
                    .await
                {
                    Ok(AppServerInteractOutcome::NotApplicable) => {
                        if self
                            .repo
                            .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnStarted);
                        }
                    }
                    Ok(
                        AppServerInteractOutcome::MintedAndAwaited { .. }
                        | AppServerInteractOutcome::RegisteredPendingForLaterAttribution { .. },
                    ) => {
                        if self
                            .repo
                            .set_phase_and_tx_output(&op, Phase::SpawnStarted, &output)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnStarted);
                        }
                    }
                    Err(e) => {
                        self.fail_with_compensation(
                            adapter.as_ref(),
                            op,
                            PhaseTag::AppServerInteract,
                            e.to_string(),
                            output,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            Phase::SpawnStarted => {
                let output = required_output(&op)?.clone();
                match adapter
                    .spawn_side_effect(&output, &op, &self.spawn_ctx)
                    .await
                {
                    Ok(SpawnOutcome::Ready(_handle)) => {
                        if self
                            .repo
                            .set_phase(&op, Phase::SpawnSucceeded)
                            .await?
                            .is_none()
                        {
                            log_lost_lease(&op, PhaseTag::SpawnSucceeded);
                        }
                    }
                    Ok(SpawnOutcome::Parked {
                        deadline_ms,
                        observer,
                    }) => {
                        if !adapter.phases().contains(&PhaseTag::Parked) {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::SpawnStarted,
                                "adapter returned parked without declaring parked phase".into(),
                                output,
                            )
                            .await?;
                            return Ok(());
                        }
                        if self.repo.set_parked(&op, deadline_ms).await?.is_some() {
                            tokio::spawn(observer);
                            return Ok(());
                        }

                        let current = self.repo.get_operation(&op.id).await?;
                        let still_holds_lease = current
                            .as_ref()
                            .map(|row| {
                                row.lease_owner == op.lease_owner
                                    && matches!(row.phase, Phase::SpawnStarted)
                            })
                            .unwrap_or(false);
                        let missing_artifacts = current
                            .as_ref()
                            .map(|row| row.spawn_artifacts.is_none())
                            .unwrap_or(false);
                        if still_holds_lease && missing_artifacts {
                            self.fail_with_compensation(
                                adapter.as_ref(),
                                op,
                                PhaseTag::SpawnStarted,
                                "adapter parked operation without recording spawn artifacts".into(),
                                output,
                            )
                            .await?;
                        } else {
                            log_lost_lease(&op, PhaseTag::Parked);
                        }
                    }
                    Err(e) => {
                        self.fail_with_compensation(
                            adapter.as_ref(),
                            op,
                            PhaseTag::SpawnStarted,
                            e.to_string(),
                            output,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            Phase::SpawnSucceeded => {
                if let Some(result) = self.repo.set_phase(&op, Phase::Succeeded).await? {
                    if let Some(result) = operation_result_from(&result)? {
                        self.completion.complete(result);
                    }
                } else {
                    log_lost_lease(&op, PhaseTag::Succeeded);
                }
                Ok(())
            }
            Phase::Compensating => {
                if let Some(result) = self
                    .resume_compensation(adapter.as_ref(), op.clone())
                    .await?
                {
                    self.completion.complete(result);
                } else {
                    log_lost_lease(&op, PhaseTag::Failed);
                }
                Ok(())
            }
            Phase::Parked => {
                tracing::warn!(
                    op_id = %op.id,
                    "parked operation reached drive_one; parked rows are excluded from drive claims"
                );
                Ok(())
            }
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. } => {
                if let Some(result) = operation_result_from(&op)? {
                    self.completion.complete(result);
                }
                Ok(())
            }
        }
    }

    async fn fail_with_compensation(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
        from_phase: PhaseTag,
        reason: String,
        output: TxOutput,
    ) -> Result<()> {
        let state = adapter
            .plan_compensation(from_phase, &reason, &output, &op)
            .await?;
        if self
            .repo
            .set_compensating(&op, &state, &output)
            .await?
            .is_none()
        {
            log_lost_lease(&op, PhaseTag::Compensating);
        }
        Ok(())
    }

    async fn resume_compensation(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
    ) -> Result<Option<OperationResult>> {
        let state = op
            .compensation_state
            .clone()
            .ok_or_else(|| {
                CalmError::Internal(format!("operation {} missing compensation_state", op.id))
            })
            .and_then(|value| {
                serde_json::from_value::<CompensationStateVersioned>(value).map_err(CalmError::from)
            })?;
        let output = required_output(&op)?.clone();
        let reason = state.reason.clone();
        let from_phase = state.from_phase;
        match self
            .apply_compensation_steps(adapter, op.clone(), state, output)
            .await
        {
            Ok(()) => {
                self.repo
                    .mark_failed(&op, reason, from_phase, Some("internal".into()))
                    .await
            }
            Err(e) => {
                self.repo
                    .mark_stuck(
                        &op,
                        format!("compensation failed: {e}"),
                        PhaseTag::Compensating,
                    )
                    .await
            }
        }
    }

    async fn apply_compensation_steps(
        &self,
        adapter: &dyn ProviderAdapter,
        op: Operation,
        mut state: CompensationStateVersioned,
        output: TxOutput,
    ) -> Result<()> {
        for idx in 0..state.steps.len() {
            if state.steps[idx].completed {
                continue;
            }
            match adapter
                .compensate_step(&state.steps[idx], &output, &op, &self.spawn_ctx)
                .await
            {
                Ok(()) => {
                    state.steps[idx].completed = true;
                    state.steps[idx].last_error = None;
                    if self
                        .repo
                        .update_compensation_state(&op, &state)
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::Compensating);
                        return Ok(());
                    }
                }
                Err(e) => {
                    state.steps[idx].attempts += 1;
                    state.steps[idx].last_error = Some(e.to_string());
                    if self
                        .repo
                        .update_compensation_state(&op, &state)
                        .await?
                        .is_none()
                    {
                        log_lost_lease(&op, PhaseTag::Compensating);
                        return Ok(());
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    async fn apply_parked_sweep(&self, op: Operation) -> Result<()> {
        self.apply_parked_sweep_with_claim(op, ParkedClaimMode::SteadyState)
            .await
    }

    async fn sweep_parked_for_boot(&self) -> Result<()> {
        self.sweep_parked_with_claim(ParkedClaimMode::Boot).await
    }

    async fn sweep_parked_with_claim(&self, claim_mode: ParkedClaimMode) -> Result<()> {
        let rows = self.repo.parked_operations().await?;
        for op in rows {
            if let Err(e) = self
                .apply_parked_sweep_with_claim(op.clone(), claim_mode)
                .await
            {
                tracing::error!(
                    op_id = %op.id,
                    error = %e,
                    "parked operation sweep failed; continuing"
                );
            }
        }
        Ok(())
    }

    async fn claim_parked_with_mode(
        &self,
        op_id: &str,
        claim_mode: ParkedClaimMode,
    ) -> Result<Option<Operation>> {
        match claim_mode {
            ParkedClaimMode::SteadyState => self.repo.claim_parked(op_id).await,
            ParkedClaimMode::Boot => self.repo.claim_parked_for_boot(op_id).await,
        }
    }

    async fn apply_parked_sweep_with_claim(
        &self,
        op: Operation,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        if !matches!(op.phase, Phase::Parked) {
            return Ok(());
        }
        let Some(deadline_ms) = op.parked_deadline_ms else {
            if let Some(claimed) = self.claim_parked_with_mode(&op.id, claim_mode).await? {
                self.fail_claimed_parked(
                    claimed,
                    "parked operation missing deadline".into(),
                    Some("parked_deadline".into()),
                )
                .await?;
            }
            return Ok(());
        };
        if now_ms() > deadline_ms {
            return self
                .apply_parked_past_deadline_with_claim(&op.id, claim_mode)
                .await;
        }
        self.apply_parked_pre_deadline_probe(op, claim_mode).await
    }

    async fn apply_parked_pre_deadline_probe(
        &self,
        op: Operation,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        let Some(artifacts) = op.spawn_artifacts.clone() else {
            return Ok(());
        };
        if parked_artifacts_alive(&artifacts) {
            return Ok(());
        }
        let adapter = self.adapter(&op.kind)?;
        match adapter
            .recover_parked(
                &op,
                &artifacts,
                false,
                RecoveryMode::PreDeadlineProbe,
                &self.spawn_ctx,
            )
            .await?
        {
            ParkedRecovery::Complete(outcome) => {
                self.complete_parked_and_publish(&op.id, &outcome).await?;
            }
            // Dead work with NO recoverable outcome fails now (PR #685
            // round-2 F2): leaving it parked would sit until
            // `parked_deadline_ms` and then be misclassified as a
            // deadline failure (class `parked_deadline` — for the gate
            // adapter, `gate-timeout` instead of the true
            // `gate-infra`). Class `parked_dead` matches the boot-arm
            // semantics for the same state. Racing a live observer's
            // in-flight completion is interlocked exactly like the
            // past-deadline arm (#653 §4.4 orderings): a verdict that
            // commits first makes this claim miss; once the claim
            // lands, the lease-fenced `mark_failed` wins and the
            // observer's completion rolls back on `AlreadyResolved`.
            ParkedRecovery::Fail { reason } => {
                let Some(claimed) = self.claim_parked_with_mode(&op.id, claim_mode).await? else {
                    return Ok(());
                };
                let alive = parked_artifacts_alive(&artifacts);
                self.kill_recheck_then_fail_parked(
                    claimed,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    reason,
                    Some("parked_dead".into()),
                )
                .await?;
            }
            ParkedRecovery::LeaveParked => {}
        }
        Ok(())
    }

    async fn apply_parked_past_deadline_with_claim(
        &self,
        op_id: &str,
        claim_mode: ParkedClaimMode,
    ) -> Result<()> {
        let Some(op) = self.claim_parked_with_mode(op_id, claim_mode).await? else {
            return Ok(());
        };
        let Some(artifacts) = op.spawn_artifacts.clone() else {
            return self
                .fail_claimed_parked(
                    op,
                    "parked operation missing spawn artifacts".into(),
                    Some("parked_deadline".into()),
                )
                .await;
        };
        let adapter = self.adapter(&op.kind)?;
        let alive = parked_artifacts_alive(&artifacts);
        match adapter
            .recover_parked(
                &op,
                &artifacts,
                alive,
                RecoveryMode::PastDeadline,
                &self.spawn_ctx,
            )
            .await?
        {
            ParkedRecovery::Complete(outcome) => {
                kill_parked_group_if_alive(&artifacts, alive);
                self.complete_parked_and_publish(&op.id, &outcome).await?;
            }
            ParkedRecovery::Fail { reason } => {
                self.kill_recheck_then_fail_parked(
                    op,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    reason,
                    Some("parked_deadline".into()),
                )
                .await?;
            }
            ParkedRecovery::LeaveParked => {
                tracing::warn!(
                    op_id = %op.id,
                    "adapter returned LeaveParked during past-deadline enforcement"
                );
                self.kill_recheck_then_fail_parked(
                    op,
                    adapter.as_ref(),
                    artifacts,
                    alive,
                    "parked deadline exceeded".into(),
                    Some("parked_deadline".into()),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn complete_parked_and_publish(
        &self,
        op_id: &OperationId,
        outcome: &ParkedOutcome,
    ) -> Result<Option<OperationResult>> {
        let pool = self.repo.sqlite_pool();
        let mut tx = begin_immediate_tx(&pool).await?;
        match complete_parked_tx(&mut tx, op_id, outcome).await? {
            ParkedCompletion::Completed(result) => {
                tx.commit().await?;
                self.publish_completion(result.clone());
                Ok(Some(result))
            }
            ParkedCompletion::AlreadyResolved { .. } => {
                tx.rollback().await?;
                Ok(None)
            }
        }
    }

    async fn kill_recheck_then_fail_parked(
        &self,
        op: Operation,
        adapter: &dyn ProviderAdapter,
        artifacts: SpawnArtifacts,
        alive: bool,
        reason: String,
        last_error_class: Option<String>,
    ) -> Result<()> {
        if alive {
            kill_parked_group_if_alive(&artifacts, true);
            match adapter
                .recover_parked(
                    &op,
                    &artifacts,
                    false,
                    RecoveryMode::PastDeadline,
                    &self.spawn_ctx,
                )
                .await?
            {
                ParkedRecovery::Complete(outcome) => {
                    self.complete_parked_and_publish(&op.id, &outcome).await?;
                    return Ok(());
                }
                ParkedRecovery::Fail { .. } | ParkedRecovery::LeaveParked => {}
            }
        }
        if let Some(result) = self
            .repo
            .mark_failed(&op, reason, PhaseTag::Parked, last_error_class)
            .await?
        {
            self.publish_completion(result);
        } else {
            log_lost_lease(&op, PhaseTag::Failed);
        }
        Ok(())
    }

    async fn fail_claimed_parked(
        &self,
        op: Operation,
        reason: String,
        last_error_class: Option<String>,
    ) -> Result<()> {
        if let Some(result) = self
            .repo
            .mark_failed(&op, reason, PhaseTag::Parked, last_error_class)
            .await?
        {
            self.publish_completion(result);
        } else {
            log_lost_lease(&op, PhaseTag::Failed);
        }
        Ok(())
    }

    async fn plan_recovery_for(
        &self,
        _adapter: &dyn ProviderAdapter,
        op: &Operation,
    ) -> Result<RecoveryItem> {
        let item = match &op.phase {
            Phase::Pending
            | Phase::TxCommitted
            | Phase::AppServerInteract { .. }
            | Phase::SpawnStarted
            | Phase::SpawnSucceeded => RecoveryItem::Recover {
                op_id: op.id.clone(),
                from_phase: op.phase.clone(),
                action: format!("drive from {}", op.phase.tag().as_str()),
            },
            Phase::Parked => RecoveryItem::VerifyParked {
                op_id: op.id.clone(),
            },
            Phase::Compensating => RecoveryItem::Compensate {
                op_id: op.id.clone(),
                reason: op
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "resume compensation".into()),
            },
            Phase::Succeeded | Phase::Failed | Phase::Stuck { .. } => RecoveryItem::Skip {
                op_id: op.id.clone(),
                reason: "terminal state".into(),
            },
        };
        Ok(item)
    }

    async fn apply_recovery_item(&self, item: RecoveryItem) -> Result<()> {
        match item {
            RecoveryItem::Recover {
                op_id, from_phase, ..
            } => {
                let Some(op) = self.repo.claim_operation_for_recovery(&op_id).await? else {
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                if let Err(e) = self.drive_one(adapter, op.clone()).await {
                    if let Some(result) = self
                        .repo
                        .mark_stuck(
                            &op,
                            format!("operation recovery apply failed: {e}"),
                            from_phase.tag(),
                        )
                        .await?
                    {
                        self.completion.complete(result);
                    } else {
                        log_lost_lease(&op, PhaseTag::Stuck);
                    }
                }
                Ok(())
            }
            RecoveryItem::Compensate { op_id, .. } => {
                let Some(op) = self.repo.claim_operation_for_recovery(&op_id).await? else {
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                match self.resume_compensation(adapter.as_ref(), op.clone()).await {
                    Ok(Some(result)) => self.completion.complete(result),
                    Ok(None) => log_lost_lease(&op, PhaseTag::Failed),
                    Err(e) => {
                        if let Some(result) = self
                            .repo
                            .mark_stuck(
                                &op,
                                format!("operation compensation recovery failed: {e}"),
                                PhaseTag::Compensating,
                            )
                            .await?
                        {
                            self.completion.complete(result);
                        } else {
                            log_lost_lease(&op, PhaseTag::Stuck);
                        }
                    }
                }
                Ok(())
            }
            RecoveryItem::VerifyParked { op_id } => {
                let Some(op) = self.repo.get_operation(&op_id).await? else {
                    return Ok(());
                };
                if !matches!(op.phase, Phase::Parked) {
                    return Ok(());
                }
                if op
                    .parked_deadline_ms
                    .is_some_and(|deadline| now_ms() > deadline)
                {
                    self.apply_parked_past_deadline_with_claim(&op_id, ParkedClaimMode::Boot)
                        .await?;
                    return Ok(());
                }
                let Some(artifacts) = op.spawn_artifacts.clone() else {
                    if let Some(claimed) = self
                        .claim_parked_with_mode(&op_id, ParkedClaimMode::Boot)
                        .await?
                    {
                        self.fail_claimed_parked(
                            claimed,
                            "parked operation missing spawn artifacts".into(),
                            Some("parked_dead".into()),
                        )
                        .await?;
                    }
                    return Ok(());
                };
                let adapter = self.adapter(&op.kind)?;
                let alive = parked_artifacts_alive(&artifacts);
                match adapter
                    .recover_parked(&op, &artifacts, alive, RecoveryMode::Boot, &self.spawn_ctx)
                    .await?
                {
                    ParkedRecovery::LeaveParked => {
                        self.repo.clear_parked_lease_for_boot(&op_id).await?;
                    }
                    ParkedRecovery::Complete(outcome) => {
                        kill_parked_group_if_alive(&artifacts, alive);
                        self.complete_parked_and_publish(&op_id, &outcome).await?;
                    }
                    ParkedRecovery::Fail { reason } => {
                        let Some(claimed) = self
                            .claim_parked_with_mode(&op_id, ParkedClaimMode::Boot)
                            .await?
                        else {
                            return Ok(());
                        };
                        let alive = parked_artifacts_alive(&artifacts);
                        self.kill_recheck_then_fail_parked(
                            claimed,
                            adapter.as_ref(),
                            artifacts,
                            alive,
                            reason,
                            Some("parked_dead".into()),
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            RecoveryItem::Skip { .. } => Ok(()),
        }
    }
}

fn log_lost_lease(op: &Operation, intended_phase: PhaseTag) {
    tracing::warn!(
        op_id = %op.id,
        intended_phase = intended_phase.as_str(),
        "operation transition skipped because driver lost lease"
    );
}

fn kill_parked_group_if_alive(artifacts: &SpawnArtifacts, alive: bool) {
    if !alive {
        return;
    }
    if parked_artifacts_alive(artifacts) {
        signal_process_group(artifacts.pgid, libc::SIGKILL);
    }
}

fn client_failure_parts(error: &CalmError) -> Option<(String, &'static str)> {
    match error {
        CalmError::BadRequest(message) => Some((message.clone(), "bad_request")),
        CalmError::NotFound(message) => Some((message.clone(), "not_found")),
        CalmError::Forbidden(message) => Some((message.clone(), "forbidden")),
        CalmError::Conflict(message) => Some((message.clone(), "conflict")),
        CalmError::Unauthorized => Some(("unauthorized".into(), "unauthorized")),
        // PR2: extend when codex/claude adapters land and can raise
        // plugin/reset-specific client errors from prepare-time validation.
        _ => None,
    }
}
