//! Production authentication and the original ordered recovery boundary.
use crate as calm_server;
use crate::auth::{AuthState, LegacyPrivateExecution, ValidatedApplicationAuth};
use crate::state::AppState;

/// Required products of the same validated production bootstrap. Replay/from_parts
/// cannot construct the private execution declaration.
pub struct BootedApplication {
    state: AppState,
    auth: AuthState,
    execution: LegacyPrivateExecution,
}
impl BootedApplication {
    pub fn into_parts(self) -> (AppState, AuthState) {
        (self.state, self.auth)
    }

    /// Keep the original native/context/operation order and each owner's downgrade
    /// policy. Completion is distinct from native readiness.
    pub(crate) async fn recover(
        state: AppState,
        authentication: ValidatedApplicationAuth,
    ) -> anyhow::Result<Self> {
        let application = Self {
            state,
            auth: authentication.state,
            execution: authentication.execution,
        };
        let state = application.state.clone();
        calm_server::assert_worker_sessions_card_id_complete_on_boot(&state).await?;

        // Refuse to serve on an ambiguous `area_folders` table: folder resolution would hand a
        // track to an arbitrary area.
        calm_server::assert_area_folders_disjoint_on_boot(&state).await?;

        // Shared codex app-server boot/takeover failures are logged so boot can still bind.
        if let Err(e) = calm_server::boot_harnesses(&state).await {
            tracing::warn!(
                error = %e,
                "planner harness boot recovery failed; continuing without recovered harness tasks"
            );
        }
        // #1817: one availability check per Planner provider, off the boot path; a provider that
        // cannot run is a warning, never a boot failure. The handle is detached on purpose.
        drop(calm_server::agent_providers::spawn_boot_check(&state));
        // #2516: the installed Claude version, read at boot and every 10 minutes, off the boot path.
        // The handle is detached on purpose.
        drop(calm_server::claude_version::spawn_periodic_refresh(&state));

        let stale_terminals = calm_server::reconcile_supervisor_on_boot(&state).await;

        if let Err(e) = calm_server::worker_flow::start_on_boot(&state).await {
            tracing::warn!(
                error = %e,
                "worker-flow recorder boot start failed; capture stream disabled this boot"
            );
        }

        if let Err(e) = calm_server::task_context_sweep_on_boot(&state).await {
            tracing::warn!(
                error = %e,
                "task context boot sweep failed; recovery gates remain closed"
            );
        }

        calm_server::recover_operations_on_boot(&state).await?;

        // #2516: after operation recovery, one `claude-restart` per owner-created Claude card whose
        // PTY the reconcile above found lost. Off the boot path; the handle is detached on purpose.
        drop(calm_server::claude_auto_resume::spawn_on_boot(
            &state,
            stale_terminals,
        ));

        calm_server::reaper_on_boot();

        // Scheduler boot sweep. Must follow operation recovery.
        calm_server::scheduler_sweep_on_boot(&state).await;
        let daily_state =
            <calm_server::state::RouteState as axum::extract::FromRef<AppState>>::from_ref(&state);
        if let Err(error) =
            calm_server::daily_planner::reconcile(&daily_state, chrono::Utc::now()).await
        {
            tracing::warn!(%error, "daily Planner boot reconciliation failed; periodic retry remains enabled");
        }
        calm_server::daily_planner::spawn(daily_state);

        state
            .dispatcher
            .activate_legacy_execution(application.execution.clone())
            .await?;
        Ok(application)
    }
}
