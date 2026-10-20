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
        self.adapter.retained_source()?.0.check_current()
    }

    async fn validate_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        let current = self.adapter.retained_target_tx(tx, &self.wire).await?;
        require_restorable(&current)?;
        if current.binding != self.expected.binding
            || current.fence != self.expected.fence
            || current.track_id != self.expected.track_id
            || !current
                .inputs
                .refs()
                .starts_with(self.expected.inputs.refs())
        {
            return Err(CalmError::Conflict(
                "retained recovery target changed".into(),
            ));
        }
        let track =
            crate::db::sqlite::track_get_tx(tx, &TrackId::from(current.track_id.clone())).await?;
        if track.workspace != self.workspace {
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
    let held_control = !PlannerFactory::canonical_recovery_requires_idle(&target.binding.provider)
        && frame.canonical_inputs.active.is_some()
        && matches!(
            frame.header.phase,
            HarnessPhaseTag::IssuingTurn | HarnessPhaseTag::TurnRunning
        );
    if !(idle || target.inputs.is_unattempted_initial() || held_control)
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
        let expected = self.retained_target(wire).await?;
        require_restorable(&expected)?;
        let track = self
            .repo
            .track_get(&expected.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation track".into()))?;
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
