//! Bounded background settlement of reported read tasks after positive provider stop evidence.
use super::*;
use crate::operation::workspace_lease::{ReleaseDelivery, task_guard};
impl Scheduler {
    pub(super) fn start_read_guard_settlement(self: &Arc<Self>) {
        let Ok(guard) = Arc::clone(&self.read_settlement).try_lock_owned() else {
            return;
        };
        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            let _guard = guard;
            if let Some(pool) = scheduler.repo.sqlite_pool() {
                if let Err(error) =
                    crate::operation::forge_action_adapter::lifecycle::reconcile(&pool).await
                {
                    tracing::warn!(%error,"Forge reference reconciliation failed");
                }
            }
            scheduler.settle_native_execution_guards().await;
            scheduler.settle_read_guards().await;
        });
    }
    async fn settle_native_execution_guards(self: &Arc<Self>) {
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let Some(runtime) = self.operation_runtime.upgrade() else {
            return;
        };
        let Some(shared) = runtime.shared_codex() else {
            return;
        };
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"
SELECT lease_id,track_id FROM workspace_leases WHERE holder_kind='native'
AND holder_phase IN ('issuing','running','stopping') AND native_provider='codex'
AND state='held' ORDER BY updated_at_ms,lease_id LIMIT 16
"#,
        )
        .fetch_all(&pool)
        .await;
        let Ok(rows) = rows else {
            return;
        };
        for (lease, track) in rows {
            if sqlx::query(
                "UPDATE workspace_leases SET updated_at_ms=?2 WHERE lease_id=?1 AND state='held'",
            )
            .bind(&lease)
            .bind(now_ms())
            .execute(&pool)
            .await
            .is_err()
            {
                continue;
            }
            match tokio::time::timeout(
                self.worker_idle.probe_timeout,
                shared.reconcile_native_workspace_guard(&lease),
            )
            .await
            {
                Ok(Ok(true)) => self.poke(TrackId::from(track)),
                Ok(Err(error)) => {
                    tracing::debug!(%error,%lease,"native execution guard retained for recovery")
                }
                _ => {}
            }
        }
    }
    async fn settle_read_guards(&self) {
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let rows = sqlx::query_as::<_, (String, String, String)>(
            r#"
            SELECT l.lease_id,l.card_id,s.thread_id FROM workspace_leases l
            JOIN operations o ON o.id=l.lease_owner JOIN tasks t ON t.id=o.idempotency_key
            JOIN worker_sessions s ON s.spawn_op_id=o.id
            WHERE l.access_mode='read_only' AND l.state='held'
              AND t.status IN ('done','failed','canceled') AND s.thread_id IS NOT NULL
            ORDER BY l.updated_at_ms,l.lease_id LIMIT 16
        "#,
        )
        .fetch_all(&pool)
        .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error,"read settlement scan failed");
                return;
            }
        };
        for (lease, card, thread) in rows {
            if sqlx::query("UPDATE workspace_leases SET updated_at_ms=?1 WHERE lease_id=?2")
                .bind(now_ms())
                .bind(&lease)
                .execute(&pool)
                .await
                .is_err()
            {
                continue;
            }
            let facts = match tokio::time::timeout(
                self.worker_idle.probe_timeout,
                self.worker_idle.probe.read_liveness_facts(&thread),
            )
            .await
            {
                Ok(Some(facts)) => facts,
                _ => continue,
            };
            let active = self.worker_idle.probe.active_turn_id_for_thread(&thread);
            if !task_guard::turn_stopped(&facts, active.as_deref()) {
                continue;
            }
            if !matches!(
                tokio::time::timeout(
                    self.worker_idle.probe_timeout,
                    self.worker_idle.probe.background_terminals_stopped(&thread),
                )
                .await,
                Ok(Some(true))
            ) {
                continue;
            }
            if task_guard::record_read_stop(&pool, &card).await.is_err() {
                continue;
            }
            if let Err(error) = release_workspace_lease_for_card_repo(
                self.repo.as_ref(),
                &self.events,
                &card,
                ReleaseDelivery::CommitAsTaskEnded,
            )
            .await
            {
                tracing::warn!(%error,%card,"read settlement release failed");
            }
        }
    }
}

#[cfg(test)]
mod tests;
