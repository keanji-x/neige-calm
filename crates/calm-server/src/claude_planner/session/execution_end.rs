//! Session settlement uses the provider claim and boot proof; retained drivers also route control.
use super::*;

#[derive(Clone)]
pub(in crate::claude_planner) enum Association {
    Historical,
    Work(crate::harness::backend::ExecutionInput),
    Maintenance(crate::harness::backend::MaintenanceTarget),
}

use crate::harness::retained_join::RetainedJoin;

pub(super) struct DriverSlot {
    completion: RetainedJoin<bool>,
    thread: Uuid,
    turn: Uuid,
    execution: Association,
    provider_execution_id: workspace_runtime::execution::ExecutionId,
}

impl DriverSlot {
    pub(super) fn new(
        driver: tokio::task::JoinHandle<bool>,
        thread: Uuid,
        turn: Uuid,
        execution: Association,
        provider_execution_id: workspace_runtime::execution::ExecutionId,
    ) -> Self {
        Self {
            completion: RetainedJoin::new(driver),
            thread,
            turn,
            execution,
            provider_execution_id,
        }
    }

    fn matches(&self, target: &crate::harness::backend::ExecutionEndTarget) -> bool {
        use crate::harness::backend::ExecutionEndScope;
        match &target.scope {
            ExecutionEndScope::LocalProcess {
                thread_id,
                local_turn_id,
            } => {
                self.thread == *thread_id
                    && self.turn == *local_turn_id
                    && match &self.execution {
                        Association::Work(input) => input.matches(target),
                        Association::Historical | Association::Maintenance(_) => false,
                    }
            }
            ExecutionEndScope::NativeTurn { .. } | ExecutionEndScope::ReceiptProcess { .. } => {
                false
            }
        }
    }

    async fn finish(&mut self) -> Result<bool> {
        self.completion
            .finish()
            .await
            .copied()
            .map_err(|()| CalmError::Conflict("Claude driver did not finish normally".into()))
    }
}

pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = shared.driver.lock().await.as_mut() {
        driver.finish().await?;
    }
    Ok(())
}

impl ClaudePlannerSession {
    /// The issuance owner excludes starts not yet installed. A NULL provider claim
    /// proves registered executions settled; the boot sweep covers pre-C4 processes.
    pub(crate) async fn read_settled(
        &self,
        card: &str,
        thread: &str,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<bool> {
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        if !self.settlement_binding_matches(card, thread).await? {
            return Ok(false);
        }
        if !self.registered_execution_absent().await?
            || !params.host.pre_boot_absent(&params.worker_session_id)
        {
            return Ok(false);
        }
        admission.check_local().await?;
        Ok(true)
    }

    /// Caller holds `issue`; recovery without a retained driver needs a live local owner.
    async fn settlement_binding_matches(&self, card: &str, thread: &str) -> Result<bool> {
        let shared = &self.shared;
        let params = &shared.params;
        if card != params.card_id
            || shared.installed.get().is_none()
            || shared.state().shutting_down
            || params.seals.is_sealed(thread)
        {
            return Ok(false);
        }
        let Some(live) = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await?
        else {
            return Ok(false);
        };
        if crate::harness::profile::local_binding(
            params.repo.as_ref(),
            &live,
            &calm_types::runtime::AgentProvider::Claude,
            card,
            &params.track_id,
            thread,
        )
        .await?
        .is_none()
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub(in crate::claude_planner) fn owns_maintenance_target(
        &self,
        target: &crate::harness::backend::MaintenanceTarget,
    ) -> bool {
        let params = &self.shared.params;
        target.card_id == params.card_id
            && target.track_id == params.track_id
            && target.worker_session_id == params.worker_session_id
    }

    pub(crate) async fn observe_maintenance(
        &self,
        target: &crate::harness::backend::MaintenanceTarget,
    ) -> Result<Option<crate::harness::backend::MaintenanceObservation>> {
        if !self.owns_maintenance_target(target) {
            return Ok(None);
        }
        let mut slot = self.shared.driver.lock().await;
        let Some(driver) = slot.as_mut() else {
            return Ok(None);
        };
        let matches = match &driver.execution {
            Association::Maintenance(current) => {
                current == target && driver.thread.to_string() == target.thread_id
            }
            Association::Historical | Association::Work(_) => false,
        };
        if !matches {
            return Ok(None);
        }
        let settled = if driver.completion.is_finished() {
            Some(driver.finish().await.unwrap_or(false))
        } else {
            None
        };
        Ok(Some(
            crate::harness::backend::MaintenanceObservation::Turn {
                turn_id: driver.turn.to_string(),
                settled,
            },
        ))
    }

    pub(crate) async fn owns_local_execution(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
    ) -> bool {
        let params = &self.shared.params;
        target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id)
            && self
                .shared
                .driver
                .lock()
                .await
                .as_ref()
                .is_some_and(|driver| driver.matches(target))
    }

    pub(crate) async fn qualify_execution_end(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
    ) -> Result<bool> {
        let params = &self.shared.params;
        if !target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id) {
            return Ok(false);
        }
        let thread = match &target.scope {
            crate::harness::backend::ExecutionEndScope::LocalProcess { thread_id, .. } => {
                thread_id.to_string()
            }
            crate::harness::backend::ExecutionEndScope::ReceiptProcess { .. }
            | crate::harness::backend::ExecutionEndScope::NativeTurn { .. } => return Ok(false),
        };
        let _issue = self.shared.issue.lock().await;
        if !self.registered_execution_absent().await? {
            return Ok(false);
        }
        let spawned_here = self
            .shared
            .driver
            .lock()
            .await
            .as_ref()
            .is_some_and(|driver| driver.matches(target));
        if spawned_here {
            return Ok(true);
        }
        if !self
            .settlement_binding_matches(&target.card_id, &thread)
            .await?
        {
            return Ok(false);
        }
        Ok(params.host.pre_boot_absent(&params.worker_session_id))
    }

    /// Caller holds `issue`: no admitted execution or locally active turn remains.
    async fn registered_execution_absent(&self) -> Result<bool> {
        if self.shared.state().active.is_some() {
            return Ok(false);
        }
        let params = &self.shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        Ok(calm_truth::db::sqlite::provider_execution::load(
            pool.read_pool(),
            &params.worker_session_id,
        )
        .await?
        .is_none())
    }

    pub(crate) async fn interrupt_execution(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        if !self.owns_local_execution(target).await {
            return Err(CalmError::Conflict(
                "control names another Claude execution".into(),
            ));
        }
        let (thread, turn) = target.scope.thread_and_turn();
        let slot = self
            .shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread && active.turn_id == turn)
            .map(|active| Arc::clone(&active.slot))
            .ok_or_else(|| CalmError::Conflict("Claude execution already ended".into()))?;
        super::interrupt_after_admission(&slot, admission.gate())
            .await
            .map_err(|(error, _evidence)| admission.take_refusal().unwrap_or(error))
    }

    /// Harness shutdown: refuse new turns, end the live one as `Interrupted`, and Stop its execution
    /// whether or not a turn runs. The `stop` result is returned for strict callers. A session that
    /// was never installed (an install-race loser) minted and spawned nothing, so it signals nothing:
    /// its id may belong to the winner's live turn.
    pub async fn shutdown(&self) -> Result<()> {
        self.shared.state().shutting_down = true;
        tokio::time::timeout(SHUTDOWN_SETTLE_WAIT, self.shutdown_issued())
            .await
            .map_err(|_| {
                CalmError::Conflict(
                    "Claude shutdown did not finish within its settlement budget".into(),
                )
            })?
    }
    async fn shutdown_issued(&self) -> Result<()> {
        let shared = &self.shared;
        let _issue = shared.issue.lock().await;
        if shared.installed.get().is_none() {
            return Ok(());
        }
        let slot = shared
            .state()
            .active
            .as_ref()
            .map(|active| Arc::clone(&active.slot));
        if let Some(slot) = &slot {
            slot.record(TerminalCause::Interrupted);
            slot.arm_stop(tokio::time::Instant::now());
        }
        let params = &shared.params;
        let stopped = super::super::lifecycle::stop_session(
            &params.host,
            &params.worker_session_id,
            crate::provider_execution::StopReason::Shutdown,
        )
        .await;
        let id = shared
            .driver
            .lock()
            .await
            .as_ref()
            .map(|driver| driver.provider_execution_id.clone());
        let joined = join_driver(shared).await;
        stopped?;
        self.provider_admission_gate().await?;
        if let Some(id) = id {
            shared.clear_failed_driver(&id).await?;
        }
        // Owner reconciliation may have removed this exact failed join. Recheck
        // that slot instead of returning an error cached before completion.
        if joined.is_err() {
            join_driver(shared).await?;
        }
        Ok(())
    }
}

impl Shared {
    /// Await the exact retained handle before acknowledging owner completion.
    /// The caller's completion/shutdown budget bounds this cancellation-safe join.
    pub(crate) async fn clear_failed_driver(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
    ) -> Result<()> {
        let mut slot = self.driver.lock().await;
        if let Some(driver) = slot.as_mut() {
            if driver.provider_execution_id != *id {
                return Err(CalmError::Conflict(
                    "Claude completion names another driver".into(),
                ));
            }
            if driver.finish().await.is_err() {
                *slot = None;
            }
        }
        Ok(())
    }
    pub(crate) fn finish_lost(&self, turn_id: &str, turn: serde_json::Value) {
        let active = self
            .state()
            .active
            .as_ref()
            .filter(|a| a.turn_id == turn_id)
            .map(|a| (a.thread_id.clone(), a.slot.clone()));
        if let Some((thread, slot)) = active {
            self.finish_turn(
                turn_id,
                &slot,
                [PlannerEvent {
                    thread_id: Some(thread),
                    kind: provider::events::PlannerEventKind::TurnCompleted { turn },
                }],
            );
            slot.settled.send_replace(true);
        }
    }
}

impl ClaudePlannerSession {
    pub(crate) async fn complete_lost_execution(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
        outcome: serde_json::Value,
    ) -> Result<()> {
        let shared = &self.shared;
        {
            let slot = shared.driver.lock().await;
            if slot.as_ref().is_some_and(|driver| {
                driver.provider_execution_id != *id || driver.turn.to_string() != turn
            }) {
                return Err(CalmError::Conflict(
                    "Claude completion identity changed".into(),
                ));
            }
        }
        shared.clear_failed_driver(id).await?;
        let params = &shared.params;
        let row = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await?
            .ok_or_else(|| CalmError::Conflict("Claude completion session missing".into()))?;
        let thread = row
            .thread_id
            .as_deref()
            .ok_or_else(|| CalmError::Conflict("Claude completion thread missing".into()))?;
        // Completion reconciles the retained owner's identity even after its
        // authority is revoked; it does not perform a fresh issuance admission.
        if row.provider != calm_types::worker::WorkerProviderKind::Claude
            || row.card_id.as_ref().map(|id| id.as_str()) != Some(params.card_id.as_str())
            || row.track_id.as_str() != params.track_id
            || shared
                .state()
                .active
                .as_ref()
                .is_some_and(|active| active.turn_id == turn && active.thread_id != thread)
            || row
                .agent_session_id
                .as_deref()
                .is_some_and(|id| id != thread)
        {
            return Err(CalmError::Conflict(
                "Claude completion binding changed".into(),
            ));
        }
        shared.finish_lost(turn, outcome);
        Ok(())
    }
}

#[cfg(feature = "fixtures")]
impl ClaudePlannerSession {
    pub async fn abort_driver_for_test(&self) {
        let slot = self.shared.driver.lock().await;
        if let Some(DriverSlot {
            completion: RetainedJoin::Running(driver),
            ..
        }) = slot.as_ref()
        {
            driver.abort();
        }
    }
}

impl ClaudePlannerSession {
    pub(crate) async fn provider_transition_gate(&self) -> Result<()> {
        let _issue = self.shared.issue.lock().await;
        self.provider_admission_gate().await
    }
}

impl ClaudePlannerSession {
    pub(crate) async fn lost_maintenance_target(
        &self,
        id: &workspace_runtime::execution::ExecutionId,
        turn: &str,
    ) -> Option<crate::harness::backend::MaintenanceTarget> {
        let slot = self.shared.driver.lock().await;
        let driver = slot.as_ref().filter(|driver| {
            driver.provider_execution_id == *id && driver.turn.to_string() == turn
        })?;
        match &driver.execution {
            Association::Maintenance(target) => Some(target.clone()),
            Association::Historical | Association::Work(_) => None,
        }
    }
}

#[cfg(feature = "fixtures")]
impl ClaudePlannerSession {
    pub async fn wait_driver_for_test(&self) -> bool {
        match self.shared.driver.lock().await.as_mut() {
            Some(driver) => driver.finish().await.unwrap_or(false),
            None => false,
        }
    }
}
