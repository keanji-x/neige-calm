use std::time::Duration;

use calm_truth::TruthError;
use calm_truth::db::rows::WorkerFlowCursor;
use calm_truth::db::sqlite::is_sqlite_busy;
use calm_truth::db::{RepoOutOfDomain, RepoRead};
use calm_types::error::CoreError;

use crate::model::now_ms;

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
    repo.worker_flow_cursor_get(card_id, source_kind)
        .await
        .map_err(|e| CoreError::Internal(format!("worker_flow_cursor_get: {e}")))
}

/// The fields a source moves; equal values mean the stored row is already current.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Position {
    record_index: i64,
    byte_offset: i64,
    last_source_uuid: Option<String>,
    last_line_hash: Option<String>,
}

/// Writes one card's capture cursor for one source file. A source re-reads its file every poll, so
/// only a cursor that moved is written, and SQLite writer contention is waited out instead of
/// ending the capture (#1579).
pub struct CursorWriter {
    card_id: String,
    source_kind: &'static str,
    source_path: String,
    stored: Option<Position>,
}

impl CursorWriter {
    /// `stored` is the row `get` returned for this source file, if any.
    pub fn new(
        card_id: &str,
        source_kind: &'static str,
        source_path: &str,
        stored: Option<&WorkerFlowCursor>,
    ) -> Self {
        Self {
            card_id: card_id.to_string(),
            source_kind,
            source_path: source_path.to_string(),
            stored: stored.map(|row| Position {
                record_index: row.record_index,
                byte_offset: row.byte_offset,
                last_source_uuid: row.last_source_uuid.clone(),
                last_line_hash: row.last_line_hash.clone(),
            }),
        }
    }

    pub async fn persist<R>(
        &mut self,
        repo: &R,
        record_index: i64,
        byte_offset: i64,
        last_source_uuid: Option<&str>,
        last_line_hash: Option<&str>,
    ) -> Result<(), CoreError>
    where
        R: RepoOutOfDomain + ?Sized,
    {
        let next = Position {
            record_index,
            byte_offset,
            last_source_uuid: last_source_uuid.map(str::to_string),
            last_line_hash: last_line_hash.map(str::to_string),
        };
        if self.stored.as_ref() == Some(&next) {
            return Ok(());
        }
        loop {
            match repo
                .worker_flow_cursor_upsert(
                    &self.card_id,
                    self.source_kind,
                    &self.source_path,
                    record_index,
                    byte_offset,
                    last_source_uuid,
                    last_line_hash,
                    now_ms(),
                )
                .await
            {
                Ok(()) => break,
                // A single autocommit statement holds nothing when it fails, so retrying it is safe.
                Err(TruthError::Db(err)) if is_sqlite_busy(&err) => {
                    tracing::warn!(
                        card_id = %self.card_id,
                        source_kind = self.source_kind,
                        error = %err,
                        "worker-flow cursor write met SQLite writer contention; retrying"
                    );
                    tokio::time::sleep(WRITER_CONTENTION_RETRY_DELAY).await;
                }
                Err(err) => {
                    return Err(CoreError::Internal(format!(
                        "worker_flow_cursor_upsert: {err}"
                    )));
                }
            }
        }
        self.stored = Some(next);
        Ok(())
    }
}
