//! Current-source restoration through the protocol owner and the original retained builder.
//! Reopening control never grants another dispatch of an attempted input.
use super::*;

struct RetainedRecovery {
    adapter: PlannerHarnessStartAdapter,
    wire: RetainedPayload,
    expected: Target,
    workspace: crate::model::TrackWorkspace,
}

#[async_trait::async_trait]
impl crate::harness::backend::factory::ActiveRecoveryAdmission for RetainedRecovery {
    fn check_current(&self) -> Result<()> {
        crate::workspace_service::validate_runtime_workspace(&self.workspace)?;
        self.adapter.retained_source()?.0.check_current()
    }

    async fn accepted_native_end_tx(
        &self,
        tx: &mut Tx<'_>,
    ) -> Result<Option<crate::harness::input_dispatch::end_transition::AcceptedRecovery>> {
        self.validate_tx(tx).await?;
        if !self.expected.accepted_native_end {
            return Ok(None);
        };
        let carrier = conversation::current_input_fact_carrier_tx(
            tx,
            &self.wire.card_id,
            &self.wire.worker_session_id,
        )
        .await?;
        let owner = crate::conversation_inputs::authorize_resource_tx(
            tx,
            self.adapter.retained_source()?.0,
            &carrier,
        )
        .await?;
        crate::harness::input_dispatch::end_transition::accepted_recovery_tx(
            tx,
            &self.wire.card_id,
            &self.wire.worker_session_id,
            &owner,
        )
        .await
    }
    async fn validate_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        let current = self.adapter.retained_admit_tx(tx, &self.wire).await?;
        require_restorable(&current)?;
        if current.binding != self.expected.binding
            || current.fence != self.expected.fence
            || current.track_id != self.expected.track_id
            || current.accepted_native_end != self.expected.accepted_native_end
            || current.native_backend != self.expected.native_backend
            || !current
                .inputs
                .identities()
                .starts_with(self.expected.inputs.identities())
        {
            return Err(CalmError::Conflict(
                "retained recovery target changed".into(),
            ));
        }
        if current.workspace != self.workspace {
            return Err(CalmError::Conflict(
                "retained recovery workspace changed".into(),
            ));
        }
        Ok(())
    }
}

fn require_restorable(target: &Target) -> Result<()> {
    let frame = &target.snapshot;
    let idle = frame.canonical_inputs.active.is_none()
        && matches!(
            frame.header.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        );
    let held_control =
        (!PlannerFactory::canonical_recovery_requires_idle(&target.binding.provider)
            || target.accepted_native_end)
            && frame.canonical_inputs.active.is_some()
            && matches!(
                frame.header.phase,
                HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
            );
    // Reopen only to let the existing compact owner conservatively settle its
    // in-flight guard. This grants no work input or maintenance retry.
    let held_maintenance =
        !PlannerFactory::canonical_recovery_requires_idle(&target.binding.provider)
            && crate::harness::backend::PlannerBackend::manual_compact_supported(&target.binding)
            && frame.canonical_inputs.active.is_none()
            && (frame.header.phase == HarnessPhaseTag::Compacting
                || frame
                    .header
                    .compact_outcome
                    .as_ref()
                    .is_some_and(|outcome| {
                        outcome.status == crate::harness::compact_outcome::CompactStatus::Started
                    }));
    if !(idle || target.inputs.is_unattempted_initial() || held_control || held_maintenance)
        || frame.header.pending_rewind.is_some()
        || frame.header.interruption_intent.is_some()
        || frame.header.wedged_reason.is_some()
    {
        return Err(CalmError::Conflict(
            "original execution is unresolved; history retained".into(),
        ));
    }
    Ok(())
}

impl PlannerHarnessStartAdapter {
    pub(super) async fn recover_retained(
        &self,
        wire: &RetainedPayload,
        events: &crate::event::EventBus,
        track_delete_locks: &crate::per_card_lock::KeyedLocks,
    ) -> Result<()> {
        let track = self
            .retained_workspace(wire, track_delete_locks, RetainedAdmission::Driven)
            .await?;
        let expected = self.retained_admit(wire).await?;
        require_restorable(&expected)?;
        self.require_retained_backend(&expected)?;
        if !workspace_runtime::recycle::workspace_allows_runtime_recovery(&track) {
            return Err(CalmError::Conflict(
                "Restore this conversation's workspace before resuming".into(),
            ));
        }
        let runtime = self
            .repo
            .session_projection_active_for_card(&wire.card_id)
            .await?
            .filter(|row| row.id == wire.worker_session_id)
            .ok_or_else(|| CalmError::Conflict("original carrier changed".into()))?;
        let admission = Arc::new(RetainedRecovery {
            adapter: self.clone(),
            wire: wire.clone(),
            expected,
            workspace: track.workspace,
        });
        PlannerFactory::new(
            Arc::downgrade(&self.daemon),
            &self.claude_host,
            &self.acp_host,
        )
        .resume_canonical_conversation(self.repo.as_ref(), &runtime, admission)
        .await?;
        self.retained_spawn(wire, events, track_delete_locks)
            .await?;
        Ok(())
    }
}

impl PlannerHarnessStartAdapter {
    /// Read declaration follows the same session namespace as restoration.
    pub(crate) async fn canonical_native_active_tx(
        &self,
        tx: &mut Tx<'_>,
        worker: &str,
        thread: &str,
    ) -> Result<bool> {
        use crate::db::sqlite::session_backend::{
            CodexScope, CodexScopeKey, codex_backend_scope_tx,
        };
        let CodexScope::Bound(id) =
            codex_backend_scope_tx(tx, CodexScopeKey::Session(worker)).await?
        else {
            return Ok(false);
        };
        let id = crate::agent_backend_pool::BackendId::try_from(id)
            .map_err(|_| CalmError::Conflict("invalid native backend namespace".into()))?;
        Ok(self
            .codex_backends
            .instance(&id)
            .is_some_and(|daemon| daemon.active_turn_id_for_thread(thread).is_some()))
    }
}
