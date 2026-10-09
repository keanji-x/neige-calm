use std::time::Duration;

use calm_exec::flow::{
    CaptureBatch, CaptureCheckpoint, CaptureOutcome, CapturePosition, FlowRowCtx,
    WorkerFlowItemSink,
};
use calm_truth::db::RepoRead;
use calm_truth::db::rows::WorkerFlowCursor;
use calm_types::error::CoreError;
use calm_types::worker_flow::WorkerFlowItem;
use tokio_util::sync::CancellationToken;

pub const CODEX_ROLLOUT_SOURCE_KIND: &str = "codex_rollout";

/// Wait between cursor writes while another connection holds the SQLite writer lock.
const WRITER_CONTENTION_RETRY_DELAY: Duration = Duration::from_millis(100);

pub async fn get<R>(
    repo: &R,
    card_id: &str,
    source_kind: &str,
) -> Result<Option<WorkerFlowCursor>, CoreError>
where
    R: RepoRead + ?Sized,
{
    let stored = repo
        .worker_flow_cursor_get(card_id, source_kind)
        .await
        .map_err(|e| CoreError::Internal(format!("worker_flow_cursor_get: {e}")))?;
    #[cfg(feature = "fixtures")]
    calm_truth::capture_test_seam::reach(
        card_id,
        -1,
        crate::test_seams::WorkerFlowPoint::CheckpointLoaded,
    )
    .await;
    Ok(stored)
}

/// Shared record/checkpoint writer. A false result ends this source: it must not
/// continue with a normalizer state built from a stale or cancelled batch.
pub struct CursorWriter {
    card_id: String,
    source_kind: &'static str,
    source_path: String,
    stop: CancellationToken,
    stored: CaptureCheckpoint,
}

impl CursorWriter {
    pub fn new(
        card_id: &str,
        source_kind: &'static str,
        source_path: &str,
        stored: Option<&WorkerFlowCursor>,
        stop: CancellationToken,
    ) -> Self {
        Self {
            card_id: card_id.to_owned(),
            source_kind,
            source_path: source_path.to_owned(),
            stop,
            stored: stored
                .map(CaptureCheckpoint::from)
                .unwrap_or(CaptureCheckpoint::Missing),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn persist(
        &mut self,
        sink: &dyn WorkerFlowItemSink,
        ctx: &FlowRowCtx,
        items: Vec<WorkerFlowItem>,
        record_index: i64,
        byte_offset: i64,
        last_source_uuid: Option<&str>,
        last_line_hash: Option<&str>,
    ) -> Result<bool, CoreError> {
        if self.stop.is_cancelled() {
            return Ok(false);
        }
        let next = CapturePosition {
            source_path: self.source_path.clone(),
            record_index,
            byte_offset,
            last_source_uuid: last_source_uuid.map(str::to_owned),
            last_line_hash: last_line_hash.map(str::to_owned),
        };
        if items.is_empty()
            && matches!(&self.stored, CaptureCheckpoint::Present { position, .. } if position == &next)
        {
            #[cfg(feature = "fixtures")]
            calm_truth::capture_test_seam::reach(
                &self.card_id,
                record_index,
                crate::test_seams::WorkerFlowPoint::Idle,
            )
            .await;
            return Ok(true);
        }
        let batch = CaptureBatch {
            card_id: self.card_id.clone(),
            source_kind: self.source_kind.to_owned(),
            expected: self.stored.clone(),
            next,
            items,
        };
        loop {
            if self.stop.is_cancelled() {
                return Ok(false);
            }
            // Cancellation stops subsequent batches, but must settle this one.
            // Dropping sqlx's future cannot retract an already queued COMMIT.
            let capture = sink.capture_batch(ctx, &batch);
            tokio::pin!(capture);
            let result = tokio::select! {
                biased;
                _ = self.stop.cancelled() => {
                    #[cfg(feature = "fixtures")]
                    calm_truth::capture_test_seam::reach(&self.card_id, record_index,
                        crate::test_seams::WorkerFlowPoint::CancellationSettling).await;
                    capture.await
                },
                result = &mut capture => result,
            };
            match result {
                Ok(CaptureOutcome::Applied(checkpoint)) => {
                    self.stored = checkpoint;
                    return Ok(true);
                }
                Ok(CaptureOutcome::Stale) => {
                    tracing::warn!(card_id = %self.card_id, source_kind = self.source_kind,
                        source_path = %self.source_path, record_index,
                        expected = ?self.stored,
                        "worker-flow capture checkpoint stale; source stopped, fresh attachment required");
                    return Ok(false);
                }
                Err(CoreError::ServiceUnavailable(err)) => {
                    tracing::warn!(card_id = %self.card_id, source_kind = self.source_kind,
                        error = %err, "worker-flow capture writer contention; retrying full batch");
                    tokio::select! {
                        _ = self.stop.cancelled() => return Ok(false),
                        _ = tokio::time::sleep(WRITER_CONTENTION_RETRY_DELAY) => {}
                    }
                }
                // Includes ambiguous COMMIT: end capture; its next attachment reloads
                // the durable checkpoint and rebuilds sequence/turn/normalizer state.
                Err(err) => return Err(err),
            }
        }
    }
}
