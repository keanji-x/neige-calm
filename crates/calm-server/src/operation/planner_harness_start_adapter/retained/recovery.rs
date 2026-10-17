//! Current-source restoration of a known idle carrier. This uses the original
//! exact-thread transport owner and the original retained-handle builder.
use super::*;

struct RetainedRecovery {
    adapter: PlannerHarnessStartAdapter,
    wire: RetainedPayload,
    expected: Target,
    workspace: crate::model::TrackWorkspace,
}

#[async_trait::async_trait]
impl crate::shared_codex_appserver::ActiveRecoveryAdmission for RetainedRecovery {
    async fn validate_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        let current = self.adapter.retained_target_tx(tx, &self.wire).await?;
        require_idle(&current)?;
        if current.fence != self.expected.fence
            || current.track_id != self.expected.track_id
            || !current.inputs.starts_with(&self.expected.inputs)
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

fn require_idle(target: &Target) -> Result<()> {
    let frame = &target.snapshot;
    if frame.canonical_inputs.active.is_some()
        || !matches!(
            frame.header.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        )
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
    ) -> Result<()> {
        let expected = self.retained_target(wire).await?;
        require_idle(&expected)?;
        let thread = expected
            .snapshot
            .header
            .last_thread_id
            .clone()
            .ok_or_else(|| CalmError::Conflict("original thread is missing".into()))?;
        let track = self
            .repo
            .track_get(&expected.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation track".into()))?;
        if !crate::workspace_service::workspace_recycle::workspace_allows_runtime_recovery(&track) {
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
        self.daemon
            .resume_active_conversation_admitted(&runtime, &thread, admission)
            .await?;
        self.retained_spawn(wire, events).await?;
        Ok(())
    }
}
