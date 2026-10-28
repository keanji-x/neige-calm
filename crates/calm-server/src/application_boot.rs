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
        let mut application = Self {
            state,
            auth: authentication.state,
            execution: authentication.execution,
        };
        let mut state = application.state.clone();
        let recovered = async {
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
            // Expired trash collection stays off interactive deletion fences.
            drop(calm_server::workspace_service::trash::spawn(&state));

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
            state
                .harness
                .activate_maintenance_execution(application.execution.clone())?;
            state.today_summary_issuer = Some(state.dispatcher.system_assistant_issuer(
                calm_types::system_assistant::SystemAssistantSource::TodaySummary,
            )?);
            Ok::<_, anyhow::Error>(())
        }.await;
        if let Err(error) = recovered {
            state.retire_dispatch_and_stop_provider_sweep().await?;
            return Err(error);
        }
        application.state = state;
        Ok(application)
    }
}

impl AppState {
    /// Revoke dispatch sources and synchronously stop the sweep; await confirms its exit.
    /// This does not stop the Engine or other detached boot tasks.
    pub fn retire_dispatch_and_stop_provider_sweep(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.dispatcher.retire();
        let join = self.provider_sweep.as_ref().map(|sweep| sweep.stop());
        async move {
            if let Some(join) = join {
                join.await?;
            }
            Ok(())
        }
    }
}

/// Both cleanups get the same exit window. One error never cancels the other observer.
pub async fn cleanup_window<A, B>(
    window: std::time::Duration,
    first: impl std::future::Future<Output = A>,
    second: impl std::future::Future<Output = B>,
) -> (Option<A>, Option<B>) {
    let (mut a, mut b) = (None, None);
    let _ = tokio::time::timeout(window, async {
        tokio::join!(async { a = Some(first.await) }, async {
            b = Some(second.await)
        });
    })
    .await;
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn cleanup_window_polls_both_and_keeps_waiting_after_one_error() {
        let started = tokio::time::Instant::now();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let result = cleanup_window(
            std::time::Duration::from_secs(1),
            async {
                rx.await.unwrap();
                std::future::pending::<()>().await;
            },
            async {
                tx.send(()).unwrap();
                Err::<(), _>("mobile failed")
            },
        )
        .await;
        assert_eq!(result, (None, Some(Err("mobile failed"))));
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    }
    #[tokio::test]
    async fn cleanup_window_allows_both_observers_to_complete() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let result = cleanup_window(
            std::time::Duration::from_secs(1),
            async {
                rx.await.unwrap();
                Ok::<_, ()>(())
            },
            async {
                tx.send(()).unwrap();
                Ok::<_, ()>(())
            },
        )
        .await;
        assert_eq!(result, (Some(Ok(())), Some(Ok(()))));
    }
    /// The weakest feasible constraint for crash recovery's documented ordering.
    /// It cannot catch regressions that wrap a recovery step in a detached spawn.
    #[test]
    fn recovery_steps_keep_their_documented_order() {
        let source = include_str!("application_boot.rs");
        let start = source.find("pub(crate) async fn recover(").unwrap();
        let function = &source[start..];
        let body =
            &function[function.find("{\n").unwrap() + 2..function.find("\n    }\n}").unwrap()];
        let steps = [
            "boot_harnesses(",
            "reconcile_supervisor_on_boot(",
            "task_context_sweep_on_boot(",
            "recover_operations_on_boot(",
            "claude_auto_resume::spawn_on_boot(",
            "reaper_on_boot(",
            "scheduler_sweep_on_boot(",
        ];
        let positions = steps.map(|step| body.find(step).expect(step));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn recovery_hands_the_reconcile_stale_set_to_claude_auto_resume() {
        let source = include_str!("application_boot.rs");
        let function = source.split_once("pub(crate) async fn recover(").unwrap().1;
        let body = function.split_once("\n    }\n}").unwrap().0;
        assert!(body.contains(
            "let stale_terminals = calm_server::reconcile_supervisor_on_boot(&state).await;"
        ));
        assert_eq!(body.matches("let stale_terminals").count(), 1);
        let call = body
            .split_once("claude_auto_resume::spawn_on_boot(")
            .unwrap()
            .1
            .split_once(')')
            .unwrap()
            .0;
        assert!(call.contains("stale_terminals"), "{call}");
    }
}
