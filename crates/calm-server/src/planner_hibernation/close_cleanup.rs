//! Retry a committed close through the original harness/backend lifecycle, outside SQLite.
use super::*;
use crate::db::write_in_tx_typed;
use crate::error::CalmError;
use crate::mcp_server::transport::carrier_quiescence::{IdleFence, ToolCallGuard};

use crate::track_close::PlannerCleanup as Cleanup;

impl WakeContext {
    pub(crate) async fn sweep_close_cleanups(&self) {
        let Some(db) = self.repo.sqlite_db() else {
            return;
        };
        let rows = sqlx::query_as::<_, Cleanup>(
            "SELECT worker_session_id,card_id,track_id,requested_at_ms FROM track_close_cleanups \
             ORDER BY requested_at_ms,worker_session_id",
        )
        .fetch_all(db.read_pool())
        .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "closed Planner cleanup scan failed");
                return;
            }
        };
        for row in rows {
            match self.tool_calls.fence_when_idle(&row.worker_session_id) {
                IdleFence::Acquired(fence) => {
                    let wake = self.clone();
                    tokio::spawn(async move {
                        wake.stop_under_fence(row, fence).await;
                    });
                }
                IdleFence::Waiting(idle) => {
                    let wake = self.clone();
                    tokio::spawn(async move {
                        if let Ok(fence) = idle.await {
                            wake.stop_under_fence(row, fence).await;
                        }
                    });
                }
                IdleFence::AlreadyWaiting => {}
            }
        }
    }

    async fn stop_under_fence(&self, row: Cleanup, _fence: ToolCallGuard) {
        let session = row.worker_session_id.clone();
        if let Err(error) = self.cleanup_closed_planner(row).await {
            tracing::warn!(session_id=%session, %error, "closed Planner stop unconfirmed; obligation retained");
        }
    }

    async fn cleanup_closed_planner(&self, row: Cleanup) -> Result<()> {
        let operations = self
            .operation_runtime
            .upgrade()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let route_repo: std::sync::Arc<dyn crate::db::RouteRepo> = self.repo.clone();
        let _card = CardStartFence::lock(
            &self.planner_recovery_locks,
            &route_repo,
            &operations,
            &row.card_id.clone().into(),
        )
        .await;
        let drive = operations.lock_for_track_delete().await;
        let _track = lock_key(&self.track_delete_locks, &row.track_id).await;
        let check = row.clone();
        let runtime = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { check.pending_runtime_tx(tx).await })
        })
        .await?;
        let Some(runtime) = runtime else {
            return Ok(());
        };
        // The exact card and Track still fence replacement and issuance. Other Tracks
        // must not wait for this session's provider, run loop or END observation.
        drop(drive);
        // Resolve the original provider once for both installed and detached completion.
        let native = if runtime.agent_provider
            == Some(crate::session_projection_repo::AgentProvider::Codex)
            && crate::harness::backend::factory::PlannerFactory::closed_native_target(&runtime)?
                .is_some()
        {
            let backends = self
                .backends
                .upgrade()
                .ok_or(CalmError::ExecutionUnavailable)?;
            Some(backends.for_session(&runtime.id).await?.clone())
        } else {
            None
        };
        let handle = self.harness.get(&row.worker_session_id);
        // Registry residency selects physical control, never original END recovery.
        if let Some(handle) = &handle {
            handle.shutdown_confirmed().await?;
        } else {
            crate::harness::backend::factory::PlannerFactory::new(
                self.daemon.clone(),
                &self.providers.claude,
                &self.providers.acp,
            )
            .shutdown_closed_session(&runtime, native.as_deref())
            .await?;
        }
        let detached_observer = if handle.is_none() {
            native
                .as_ref()
                .map(|native| {
                    crate::harness::backend::PlannerBackend::codex(
                        native.clone(),
                        &row.card_id,
                        &row.track_id,
                        &row.worker_session_id,
                    )
                })
                .transpose()?
        } else {
            None
        };
        let observer = handle
            .as_ref()
            .map(|handle| handle.closed_execution_observer())
            .or(detached_observer.as_ref());
        crate::harness::backend::factory::PlannerFactory::settle_closed_session(
            self.repo.as_ref(),
            &runtime,
            native.as_deref(),
            &row,
            observer,
        )
        .await?;
        if let Some(handle) = &handle {
            if !self.harness.retire_if_same(&row.worker_session_id, handle) {
                return Err(CalmError::Conflict(
                    "closed Planner owner changed; retry cleanup".into(),
                ));
            }
            handle.post_registry_shutdown_cleanup().await;
        }
        write_in_tx_typed(self.repo.as_ref(), move |tx| Box::pin(async move {
            sqlx::query(
                "DELETE FROM track_close_cleanups WHERE worker_session_id=?1 AND card_id=?2 AND track_id=?3 AND requested_at_ms=?4",
            ).bind(&row.worker_session_id).bind(&row.card_id).bind(&row.track_id).bind(row.requested_at_ms)
                .execute(&mut **tx).await?;
            Ok(())
        })).await
    }
}
