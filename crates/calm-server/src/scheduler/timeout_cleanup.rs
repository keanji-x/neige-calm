//! Retry cleanup of exactly the ended attempt named by a session's timeout marker.
use super::*;
use calm_truth::db::sqlite::{
    session_complete_tx, session_get_tx, session_projection_by_id_tx, terminal_get_by_card_tx,
    worker_op_targets_card_tx,
};
use calm_truth::worker_binding::{session_is_card_current_tx, session_owns_attempt_tx};
use calm_types::worker::{WorkerSessionId, WorkerSessionState};

#[derive(Clone, sqlx::FromRow)]
struct TimeoutCleanupSession {
    session_id: String,
    card_id: String,
    task_id: String,
    marker: String,
}

enum CleanupAuthority {
    Retain,
    Retire,
    Stop(
        Box<(
            crate::session_projection_repo::WorkerSessionProjection,
            Option<crate::model::Terminal>,
        )>,
    ),
}

impl Scheduler {
    pub(super) async fn sweep_timeout_worker_cleanups(self: &Arc<Self>) {
        let Some(pool) = self.repo.sqlite_db() else {
            return;
        };
        let rows = sqlx::query_as::<_, TimeoutCleanupSession>(
            "SELECT id AS session_id, card_id,
                    json_extract(handle_state_json, '$.timeout_cleanup.task_id') AS task_id,
                    json_extract(handle_state_json, '$.timeout_cleanup') AS marker
             FROM worker_sessions
             WHERE provider IN ('codex', 'claude') AND card_id IS NOT NULL
               AND json_extract(handle_state_json, '$.timeout_cleanup.requested_at_ms') IS NOT NULL
               AND json_type(handle_state_json, '$.timeout_cleanup.task_id') = 'text'
             ORDER BY updated_at_ms ASC, id ASC",
        )
        .fetch_all(pool.read_pool())
        .await;
        let worker_rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "scheduler sweep: timed-out worker cleanup scan failed");
                return;
            }
        };
        #[cfg(any(test, feature = "fixtures"))]
        {
            let hook = self
                .timeout_cleanup_discovered_test_hook
                .lock()
                .expect("timeout cleanup hook lock")
                .take();
            if let Some(hook) = hook {
                hook.resolved.notify_one();
                hook.resume.notified().await;
            }
        }
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return;
        };
        futures_util::future::join_all(worker_rows.into_iter().map(|cleanup| {
            let runtime = runtime.clone();
            async move {
                // Wait only for this card, before taking drive. A second pass rechecks
                // the full marker after the first finishes; it cannot stop a successor.
                let _card = crate::per_card_lock::lock_card(
                    &self.timeout_cleanup_locks, &cleanup.card_id,
                ).await;
                if let Err(error) = self.cleanup_timeout_worker(cleanup.clone(), runtime).await {
                    tracing::warn!(session_id = %cleanup.session_id, card_id = %cleanup.card_id,
                        %error, "scheduler sweep: timed-out worker cleanup failed; marker retained");
                }
            }
        })).await;
    }

    async fn cleanup_timeout_worker(
        &self,
        cleanup: TimeoutCleanupSession,
        runtime: Arc<OperationRuntime>,
    ) -> Result<()> {
        // Freeze before a producing phase can start. The durable marker then fences
        // session/terminal reuse in the existing launch owners until this exact CAS.
        let drive = runtime.lock_for_track_delete().await;
        let frozen = crate::db::write_in_tx_typed(self.repo.as_ref(), {
            let cleanup = cleanup.clone();
            move |tx| Box::pin(async move { timeout_cleanup_authority_tx(tx, &cleanup).await })
        })
        .await?;
        drop(drive);
        let complete_session = match frozen {
            CleanupAuthority::Retain => return Ok(()),
            CleanupAuthority::Retire => false,
            CleanupAuthority::Stop(worker) => {
                let (projection, terminal) = *worker;
                runtime
                    .stop_timeout_worker_session(Some(&projection), terminal.as_ref())
                    .await?;
                true
            }
        };
        // BEGIN IMMEDIATE rechecks the discovered marker and binds its release to that attempt.
        let write = self.write.clone();
        let (ids, events) =
            crate::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    // A stale marker can be retired, but is never authority to release B's lease,
                    // even when B has itself ended and even for a lease without delivery policy.
                    let cleared = sqlx::query(
                    "UPDATE worker_sessions SET handle_state_json = json_remove(handle_state_json,
                        '$.timeout_cleanup'), updated_at_ms = ?
                     WHERE id = ? AND card_id = ?
                       AND json_extract(handle_state_json, '$.timeout_cleanup') = ?")
                    .bind(now_ms()).bind(&cleanup.session_id).bind(&cleanup.card_id)
                    .bind(&cleanup.marker).execute(&mut **tx).await?.rows_affected();
                    let released = if cleared == 1 {
                        if complete_session {
                            // Shutdown may already have persisted A's exit. Complete only the
                            // exact Stop-authorized session while still live, never the card's
                            // current session and never the Retire (foreign/stale) carrier.
                            let id = WorkerSessionId::from(cleanup.session_id.clone());
                            let session = session_get_tx(tx, &id).await?.ok_or_else(|| {
                                CalmError::Internal(
                                    "CAS-cleared timeout session disappeared".into(),
                                )
                            })?;
                            if !session.state.is_terminal() {
                                session_complete_tx(
                                    tx,
                                    &cleanup.session_id,
                                    WorkerSessionState::Failed,
                                )
                                .await?;
                            }
                        }
                        release_ended_attempt_lease_for_card_tx(
                            tx,
                            &cleanup.card_id,
                            EndedAttempt::Exactly(&cleanup.task_id),
                            ReleaseDelivery::CommitAsTaskEnded,
                        )
                        .await?
                    } else {
                        Vec::new()
                    };
                    let ids = if released.is_empty() {
                        Vec::new()
                    } else {
                        crate::db::sqlite::append_actor_events_in_tx(tx, None, &write, &released)
                            .await?
                            .0
                    };
                    Ok((ids, released))
                })
            })
            .await?;
        crate::db::sqlite::broadcast_actor_events(&self.events, &ids, events);
        Ok(())
    }
}

// The marker is discovery, not stop authority. Re-read all bindings while holding the driver
// barrier and a short writer transaction. Unknown ownership retains the retry obligation.
async fn timeout_cleanup_authority_tx(
    tx: &mut Tx<'_>,
    cleanup: &TimeoutCleanupSession,
) -> Result<CleanupAuthority> {
    let marker: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(handle_state_json, '$.timeout_cleanup')
         FROM worker_sessions WHERE id = ? AND card_id = ?",
    )
    .bind(&cleanup.session_id)
    .bind(&cleanup.card_id)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    if marker.as_deref() != Some(cleanup.marker.as_str()) {
        return Ok(CleanupAuthority::Retain);
    }
    let session_id = WorkerSessionId::from(cleanup.session_id.clone());
    let Some(session) = session_get_tx(tx, &session_id).await? else {
        return Ok(CleanupAuthority::Retain);
    };
    let Some(task) = task_get_tx(tx, &cleanup.task_id).await? else {
        return Ok(CleanupAuthority::Retain);
    };
    if !task.status.is_terminal() || task.track_id != session.track_id.as_str() {
        return Ok(CleanupAuthority::Retain);
    }
    let owns_card = match task.worker_card_id.as_deref() {
        Some(card) => card == cleanup.card_id,
        None => worker_op_targets_card_tx(tx, &cleanup.task_id, &cleanup.card_id).await?,
    };
    if !owns_card {
        return Ok(CleanupAuthority::Retain);
    }
    if !session_is_card_current_tx(
        tx,
        &cleanup.card_id,
        &cleanup.session_id,
        session.track_id.as_str(),
    )
    .await?
    {
        return Ok(CleanupAuthority::Retire);
    }
    let Some(owns_attempt) =
        session_owns_attempt_tx(tx, &session, &cleanup.task_id, &cleanup.card_id).await?
    else {
        return Ok(CleanupAuthority::Retain);
    };
    if !owns_attempt {
        return Ok(CleanupAuthority::Retire);
    }
    let Some(projection) = session_projection_by_id_tx(tx, &cleanup.session_id).await? else {
        return Ok(CleanupAuthority::Retain);
    };
    let terminal = terminal_get_by_card_tx(tx, &cleanup.card_id)
        .await?
        .filter(|term| projection.terminal_run_id.as_deref() == Some(term.id.as_str()));
    crate::harness::backend::factory::PlannerFactory::require_worker_stop_binding_tx(
        tx,
        &projection,
        terminal.as_ref(),
    )
    .await?;
    Ok(CleanupAuthority::Stop(Box::new((projection, terminal))))
}
