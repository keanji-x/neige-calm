//! The only production capture write: one source record and its checkpoint.
use calm_exec::flow::{CaptureCheckpoint, CaptureOutcome};
use sqlx::{Sqlite, Transaction};

use super::SqlxRepo;
#[cfg(feature = "fixtures")]
use super::is_sqlite_busy;
use crate::db::rows::WorkerFlowCursor;
use crate::db::worker_flow_capture::WorkerFlowCapture;
use crate::error::{Result, TruthError};
use crate::model::now_ms;

impl SqlxRepo {
    pub(super) async fn capture_commit(
        &self,
        capture: &WorkerFlowCapture,
    ) -> Result<CaptureOutcome> {
        #[cfg(feature = "fixtures")]
        crate::capture_test_seam::reach(
            &capture.card_id,
            capture.next.record_index,
            crate::capture_test_seam::CapturePoint::BeforeTransaction,
        )
        .await;
        // The shared source writer owns retry/cancellation, not begin_immediate_tx's
        // bounded retry loop. A failed BEGIN owns no transaction and is safe to retry.
        let mut tx = match self.pool.begin_with("BEGIN IMMEDIATE").await {
            Ok(tx) => tx,
            Err(e) => {
                #[cfg(feature = "fixtures")]
                if is_sqlite_busy(&e) {
                    crate::capture_test_seam::reach(
                        &capture.card_id,
                        capture.next.record_index,
                        crate::capture_test_seam::CapturePoint::Busy,
                    )
                    .await;
                }
                return Err(e.into());
            }
        };
        let stored = sqlx::query_as::<_, WorkerFlowCursor>(
            r#"SELECT card_id, source_kind, source_path, record_index,
                      byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               FROM worker_flow_cursors
               WHERE card_id = ?1 AND source_kind = ?2"#,
        )
        .bind(&capture.card_id)
        .bind(&capture.source_kind)
        .fetch_optional(&mut *tx)
        .await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(e) => {
                tx.rollback().await.map_err(|rollback| {
                    TruthError::Internal(format!("capture read rollback after {e}: {rollback}"))
                })?;
                return Err(e.into());
            }
        };
        let expected = stored
            .as_ref()
            .map(CaptureCheckpoint::from)
            .unwrap_or(CaptureCheckpoint::Missing);
        if expected != capture.expected {
            tx.rollback().await?;
            return Ok(CaptureOutcome::Stale);
        }
        // This column also supplies task liveness: preserve wall-clock semantics,
        // rather than advancing it as a logical counter during rapid empty batches.
        let updated_at_ms = now_ms();
        let result = async {
            for item in &capture.items {
                super::out_of_domain::worker_flow_item_insert_tx(
                    &mut tx,
                    Some(&capture.card_id),
                    Some(capture.session_id.as_str()),
                    capture.track_id.as_deref(),
                    Some(capture.session_id.as_str()),
                    &item.kind,
                    &item.payload,
                    now_ms(),
                )
                .await?;
            }
            // All source items are uncommitted here, before checkpoint SQL.
            #[cfg(feature = "fixtures")]
            crate::capture_test_seam::reach(
                &capture.card_id,
                capture.next.record_index,
                crate::capture_test_seam::CapturePoint::ItemInserted,
            )
            .await;
            worker_flow_cursor_upsert_tx(
                &mut tx,
                &capture.card_id,
                &capture.source_kind,
                &capture.next,
                updated_at_ms,
            )
            .await
        }
        .await;
        if let Err(e) = result {
            // Only propagate retryable DB errors after confirmed full rollback.
            tx.rollback().await.map_err(|rollback| {
                TruthError::Internal(format!("capture rollback after {e}: {rollback}"))
            })?;
            return Err(e);
        }
        // A COMMIT error is ambiguous; do not classify it as safe writer contention.
        tx.commit().await.map_err(|e| {
            TruthError::Internal(format!(
                "capture commit acknowledgement uncertain; reload durable checkpoint: {e}"
            ))
        })?;
        #[cfg(feature = "fixtures")]
        crate::capture_test_seam::reach(
            &capture.card_id,
            capture.next.record_index,
            crate::capture_test_seam::CapturePoint::Committed,
        )
        .await;
        Ok(CaptureOutcome::Applied(CaptureCheckpoint::Present {
            position: capture.next.clone(),
            updated_at_ms,
        }))
    }
}

pub(super) async fn worker_flow_cursor_upsert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    source_kind: &str,
    position: &calm_exec::flow::CapturePosition,
    updated_at_ms: i64,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO worker_flow_cursors (
                   card_id, source_kind, source_path, record_index,
                   byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
               ON CONFLICT(card_id, source_kind) DO UPDATE SET
                   source_path = excluded.source_path,
                   record_index = excluded.record_index,
                   byte_offset = excluded.byte_offset,
                   last_source_uuid = excluded.last_source_uuid,
                   last_line_hash = excluded.last_line_hash,
                   updated_at_ms = excluded.updated_at_ms"#,
    )
    .bind(card_id)
    .bind(source_kind)
    .bind(&position.source_path)
    .bind(position.record_index)
    .bind(position.byte_offset)
    .bind(&position.last_source_uuid)
    .bind(&position.last_line_hash)
    .bind(updated_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
