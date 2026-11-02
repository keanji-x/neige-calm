//! Deadline scans never await Planner wake or acceptance. Delivery shares the existing
//! per-Track push lock, ingress and durable event decoder, including canonical ownership.
use super::*;

impl Inner {
    pub(super) async fn progress_watchdog_once(self: &Arc<Self>) -> crate::error::Result<()> {
        self.scheduler.sweep_progress_obligations().await?;
        let mut grouped = std::collections::BTreeMap::<String, Vec<i64>>::new();
        for notice in self.scheduler.pending_progress_notices().await? {
            grouped
                .entry(notice.track_id)
                .or_default()
                .push(notice.event_id);
        }
        for (track, ids) in grouped {
            let track = TrackId::from(track);
            let Some(guard) = self.try_acquire_push_lock(&track) else {
                continue;
            };
            let inner = self.clone();
            // Owning the existing lock before spawning bounds work to one delivery per
            // Track even when native activation holds its start fence across many ticks.
            tokio::spawn(async move {
                if let Err(error) = inner.deliver_progress_notices(&guard, ids).await {
                    tracing::warn!(%track, %error, "progress notice remains durable; next tick retries");
                }
            });
        }
        Ok(())
    }

    async fn deliver_progress_notices(
        self: &Arc<Self>,
        guard: &PushLockGuard,
        ids: Vec<i64>,
    ) -> crate::error::Result<()> {
        for id in ids {
            let rows = self.repo.events_since(id.saturating_sub(1), 1).await?;
            let Some((_, _, _, event)) = rows.into_iter().find(|row| row.0 == id) else {
                return Err(crate::error::CalmError::Internal(
                    "durable progress notice is unavailable".into(),
                ));
            };
            self.observe_harness_under_lock(guard, &event, id).await;
        }
        Ok(())
    }

    pub(super) fn spawn_progress_watchdog(self: &Arc<Self>) -> JoinHandle<()> {
        let inner = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(DEFAULT_RECONCILE_SECS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let Some(inner) = inner.upgrade() else { break };
                if let Err(error) = inner.progress_watchdog_once().await {
                    tracing::warn!(%error, "progress-obligation scan failed; next tick retries");
                }
            }
        })
    }
}
