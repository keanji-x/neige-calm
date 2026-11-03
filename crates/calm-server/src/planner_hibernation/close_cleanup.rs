//! Retry a committed close through the original harness/backend lifecycle, outside SQLite.
use super::*;
use crate::db::write_in_tx_typed;
use crate::error::CalmError;

#[derive(Clone, sqlx::FromRow)]
struct Cleanup {
    worker_session_id: String,
    card_id: String,
    track_id: String,
    requested_at_ms: i64,
}

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
            if let Err(error) = self.cleanup_closed_planner(row.clone()).await {
                tracing::warn!(session_id=%row.worker_session_id, %error, "closed Planner stop unconfirmed; obligation retained");
            }
        }
    }

    async fn cleanup_closed_planner(&self, row: Cleanup) -> Result<()> {
        // A held reply defers only this session. The transport pokes cleanup after releasing
        // its admissions; the regular sweep also retries the durable obligation.
        let Ok(_calls) = self.tool_calls.fence(&row.worker_session_id) else {
            return Ok(());
        };
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
        let _drive = operations.lock_for_track_delete().await;
        let _track = lock_key(&self.track_delete_locks, &row.track_id).await;
        let check = row.clone();
        let runtime = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let pending: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM track_close_cleanups \
                 WHERE worker_session_id=?1 AND card_id=?2 AND track_id=?3 AND requested_at_ms=?4)",
                )
                .bind(&check.worker_session_id)
                .bind(&check.card_id)
                .bind(&check.track_id)
                .bind(check.requested_at_ms)
                .fetch_one(&mut **tx)
                .await?;
                if !pending {
                    return Ok(None);
                }
                let runtime =
                    crate::db::sqlite::session_projection_by_id_tx(tx, &check.worker_session_id)
                        .await?
                        .ok_or_else(|| {
                            CalmError::Conflict("closed Planner binding unavailable".into())
                        })?;
                let session =
                    crate::db::sqlite::session_get_tx(tx, &check.worker_session_id.clone().into())
                        .await?
                        .ok_or_else(|| {
                            CalmError::Conflict("closed Planner binding unavailable".into())
                        })?;
                if runtime.card_id != check.card_id
                    || session.track_id.as_str() != check.track_id
                    || session.card_id.as_ref().map(|id| id.as_str())
                        != Some(check.card_id.as_str())
                    || session.contract != calm_types::worker::WorkerContract::Planner
                {
                    return Err(CalmError::Conflict("closed Planner binding changed".into()));
                }
                Ok(Some(runtime))
            })
        })
        .await?;
        let Some(runtime) = runtime else {
            return Ok(());
        };
        // No SQL writer is held while the run loop joins, local process stops, or native RPC runs.
        if let Some(handle) = self.harness.get(&row.worker_session_id) {
            handle.shutdown_confirmed().await?;
            if !self.harness.retire_if_same(&row.worker_session_id, &handle) {
                return Err(CalmError::Conflict(
                    "closed Planner owner changed; retry cleanup".into(),
                ));
            }
            handle.post_registry_shutdown_cleanup().await;
        } else {
            let backends = self
                .backends
                .upgrade()
                .ok_or(CalmError::ExecutionUnavailable)?;
            let native = if runtime.agent_provider
                == Some(crate::session_projection_repo::AgentProvider::Codex)
            {
                Some(backends.for_session(&runtime.id).await?)
            } else {
                None
            };
            crate::harness::backend::factory::PlannerFactory::new(
                self.daemon.clone(),
                &self.providers.claude,
                &self.providers.acp,
            )
            .shutdown_closed_session(&runtime, native.map(|b| b.as_ref()))
            .await?;
        }
        write_in_tx_typed(self.repo.as_ref(), move |tx| Box::pin(async move {
            let removed = sqlx::query(
                "DELETE FROM track_close_cleanups WHERE worker_session_id=?1 AND card_id=?2 AND track_id=?3 AND requested_at_ms=?4",
            ).bind(&row.worker_session_id).bind(&row.card_id).bind(&row.track_id).bind(row.requested_at_ms)
                .execute(&mut **tx).await?.rows_affected();
            if removed == 1 {
                let session = crate::db::sqlite::session_get_tx(tx, &row.worker_session_id.clone().into()).await?
                    .ok_or_else(|| CalmError::Internal("closed Planner session disappeared".into()))?;
                if !session.state.is_terminal() {
                    crate::db::sqlite::session_complete_tx(tx, &row.worker_session_id, calm_types::worker::WorkerSessionState::Failed).await?;
                }
            }
            Ok(())
        })).await
    }
}
