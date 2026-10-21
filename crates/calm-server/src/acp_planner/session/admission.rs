//! Current managed ACP authority, re-read under the actual transport writer.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;
use calm_types::worker::WorkerSession;

impl Shared {
    pub(super) async fn current_owner(
        &self,
        thread: &str,
        permission: Option<PlannerPermissionMode>,
    ) -> std::result::Result<WorkerSession, StartFailure> {
        let params = &self.params;
        let row = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await
            .map_err(CalmError::from)?
            .ok_or_else(|| refused("ACP owner is missing"))?;
        if !matches!(
            row.contract,
            WorkerContract::Planner | WorkerContract::Assistant
        ) {
            return Err(refused("ACP owner identity changed"));
        }
        let binding = crate::harness::profile::local_binding(
            params.repo.as_ref(),
            &row,
            &params.provider,
            &params.card_id,
            &params.track_id,
            thread,
        )
        .await?
        .ok_or_else(|| refused("ACP live owner identity changed"))?;
        if binding.profile == crate::harness::profile::HarnessProfile::Assistant
            && permission.is_some_and(|mode| mode != PlannerPermissionMode::Never)
        {
            return Err(refused("ACP live owner identity changed"));
        }
        Ok(row)
    }

    pub(super) fn admission<'a>(
        &'a self,
        thread: &'a str,
        permission: Option<PlannerPermissionMode>,
        native: Option<&'a str>,
        turn: Option<&'a str>,
    ) -> NativePromptAdmission<'a> {
        NativePromptAdmission::new(
            move || {
                let state = self.state.lock().expect("ACP state");
                if state.closed
                    || self.parked.is_parked()
                    || self.params.seals.is_sealed(thread)
                    || turn.is_some_and(|turn| {
                        state.active.as_ref().is_none_or(|active| {
                            active.thread != thread
                                || active.turn != turn
                                || native != Some(active.native_session_id.as_str())
                        })
                    })
                {
                    return Err(CalmError::Conflict(
                        "ACP conversation or execution changed before write".into(),
                    ));
                }
                Ok(())
            },
            move || {
                Box::pin(async move {
                    let row = self
                        .current_owner(thread, permission)
                        .await
                        .map_err(CalmError::from)?;
                    if row.agent_session_id.as_deref() != native {
                        return Err(CalmError::Conflict(
                            "ACP native owner changed before write".into(),
                        ));
                    }
                    Ok(())
                })
            },
        )
    }
}
