//! Retry cleanup of exactly the ended attempt named by a session's timeout marker.
use super::*;
use calm_truth::db::sqlite::{
    session_get_tx, session_projection_by_id_tx, terminal_get_by_card_tx, worker_op_targets_card_tx,
};
use calm_truth::worker_binding::{operation_idempotency_key, session_is_card_current_tx};
use calm_types::worker::WorkerSessionId;

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
        for cleanup in worker_rows {
            if let Err(error) = self
                .cleanup_timeout_worker(cleanup.clone(), runtime.clone())
                .await
            {
                tracing::warn!(session_id = %cleanup.session_id, card_id = %cleanup.card_id,
                    %error, "scheduler sweep: timed-out worker cleanup failed; marker retained");
            }
        }
    }

    async fn cleanup_timeout_worker(
        &self,
        cleanup: TimeoutCleanupSession,
        runtime: Arc<OperationRuntime>,
    ) -> Result<()> {
        // All runtime-producing operation phases share this existing barrier. In particular a
        // Legacy restart reuses its terminal row, so exact ids alone cannot fence replacement.
        // Acquire drive before SQLite, and release the writer while shutdown awaits exit writes.
        let _drive = runtime.lock_for_track_delete().await;
        let frozen = crate::db::write_in_tx_typed(self.repo.as_ref(), {
            let cleanup = cleanup.clone();
            move |tx| Box::pin(async move { timeout_cleanup_authority_tx(tx, &cleanup).await })
        })
        .await?;
        match frozen {
            CleanupAuthority::Retain => return Ok(()),
            CleanupAuthority::Retire => {}
            CleanupAuthority::Stop(worker) => {
                let (projection, terminal) = *worker;
                runtime
                    .stop_timeout_worker_session(Some(&projection), terminal.as_ref())
                    .await?;
            }
        }
        // BEGIN IMMEDIATE rechecks the discovered marker and binds its release to that attempt.
        write_with_actor_events_typed(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
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
                    Ok(((), released))
                })
            },
        )
        .await?;
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
    let owns_attempt = match session.spawn_op_id.as_deref() {
        Some(op) => {
            let key = operation_idempotency_key(&mut **tx, op).await?;
            let Some(key) = key else {
                return Ok(CleanupAuthority::Retain);
            };
            key == cleanup.task_id
        }
        // A Legacy restart has no spawn op. Only the latest stamped attempt can
        // authorize its stop; absent evidence retains both marker and lease.
        None => {
            let latest: Option<String> = sqlx::query_scalar(
                "SELECT id FROM tasks WHERE worker_card_id = ?
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
            )
            .bind(&cleanup.card_id)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(latest) = latest else {
                return Ok(CleanupAuthority::Retain);
            };
            latest == cleanup.task_id
        }
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
    Ok(CleanupAuthority::Stop(Box::new((projection, terminal))))
}
