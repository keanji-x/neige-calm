//! Durable system ingress, owned by the same serial loop as queue staging and snapshots.
use super::*;

struct Checkpoint {
    queue: DurableUserMessageCheckpoint,
    watermark: i64,
    hook_keys: VecDeque<String>,
    hook_set: HashSet<String>,
    report_hash: Option<String>,
}

impl Checkpoint {
    async fn take(inner: &Inner) -> Self {
        Self {
            queue: checkpoint_durable_user_message(inner).await,
            watermark: *inner.push_watermark.lock().await,
            hook_keys: inner.recent_hook_keys.lock().await.clone(),
            hook_set: inner.recent_hook_key_set.lock().await.clone(),
            report_hash: inner.last_report_body_sha256.lock().await.clone(),
        }
    }

    async fn restore(self, inner: &Inner) {
        restore_durable_user_message(inner, self.queue).await;
        *inner.push_watermark.lock().await = self.watermark;
        *inner.recent_hook_keys.lock().await = self.hook_keys;
        *inner.recent_hook_key_set.lock().await = self.hook_set;
        *inner.last_report_body_sha256.lock().await = self.report_hash;
    }
}

pub(super) async fn accept(inner: &Arc<Inner>, delivery: HarnessObservationDelivery) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
    let before = serde_json::to_value(snapshot_for(inner).await)?;
    let checkpoint = Checkpoint::take(inner).await;
    let id = delivery.entry.envelope_id().expect("enveloped observation");
    // Pending entries at/below the durable watermark are still pending. A retry only confirms
    // the existing snapshot; it must neither remove those entries nor append a second copy.
    if id > checkpoint.watermark {
        // The existing fold/drop/filter policy also consumes an envelope. Its watermark must
        // commit even when that policy does not append an entry.
        on_observation(inner, delivery.entry).await;
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::DURABLE_OBSERVATION_STAGED,
        inner.card_id.as_str(),
    )
    .await;
    let staged = match serde_json::to_value(snapshot_for(inner).await) {
        Ok(value) => value,
        Err(error) => {
            checkpoint.restore(inner).await;
            return Err(error.into());
        }
    };
    match persist_snapshot_bound(inner, None, None, SnapshotWriteMode::PlannerObservation).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            checkpoint.restore(inner).await;
            Err(CalmError::PlannerHarnessRuntimeSuperseded(
                "durable observation refused: Planner binding is no longer current".into(),
            ))
        }
        Err(error) => {
            // A commit error may have an unknown outcome. Read the same persisted snapshot:
            // keep committed staging, restore a known older snapshot, or stop all further writes
            // if storage cannot establish the outcome. Never acknowledge the failed operation.
            let stored = inner
                .repo
                .session_projection_by_id(&inner.worker_session_id)
                .await;
            match stored {
                Ok(Some(row)) => {
                    if row.handle_state_json.as_ref() == Some(&staged) {
                        // Confirm the complete queue/snapshot, not just a watermark or MAX(events).
                    } else if row.handle_state_json.as_ref() == Some(&before) {
                        checkpoint.restore(inner).await;
                    } else {
                        checkpoint.restore(inner).await;
                        close_observation_ingress(inner);
                    }
                }
                _ => {
                    checkpoint.restore(inner).await;
                    close_observation_ingress(inner);
                }
            }
            Err(error)
        }
    }
}
