use calm_truth::db::rows::WorkerFlowCursor;
use calm_truth::db::{RepoOutOfDomain, RepoRead};
use calm_types::error::CoreError;

pub const CODEX_ROLLOUT_SOURCE_KIND: &str = "codex_rollout";

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

#[allow(clippy::too_many_arguments)]
pub async fn upsert<R>(
    repo: &R,
    card_id: &str,
    source_kind: &str,
    source_path: &str,
    record_index: i64,
    byte_offset: i64,
    last_source_uuid: Option<&str>,
    updated_at_ms: i64,
) -> Result<(), CoreError>
where
    R: RepoOutOfDomain + ?Sized,
{
    repo.worker_flow_cursor_upsert(
        card_id,
        source_kind,
        source_path,
        record_index,
        byte_offset,
        last_source_uuid,
        updated_at_ms,
    )
    .await
    .map_err(|e| CoreError::Internal(format!("worker_flow_cursor_upsert: {e}")))
}
