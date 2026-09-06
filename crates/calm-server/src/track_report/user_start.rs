//! Admission for the fixed User start purpose, under the report writer's transaction.
use super::{ReportDoc, ReportDocOp, check_doc_rev};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::model::TrackLifecycle;
use calm_types::report_blocks;

pub(super) async fn prepare_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &TrackId,
    doc: &ReportDoc,
    key: &str,
    goal: &str,
    if_doc_rev: u64,
) -> Result<ReportDocOp> {
    if !report_blocks::tasks::key_is_valid(key) || goal.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "Enter a goal and a valid task key of at most 64 characters.".into(),
        ));
    }
    let track = crate::track_lifecycle::track_get_tx(tx, track_id).await?;
    if track.archived_at.is_some()
        || (track.lifecycle != TrackLifecycle::Draft
            && !crate::scheduler::lifecycle_allows_scheduling(track.lifecycle))
    {
        return Err(CalmError::Conflict(
            "This Track cannot start a task. Reopen or resume the Track first.".into(),
        ));
    }
    check_doc_rev(doc, if_doc_rev).map_err(|error| match error {
        CalmError::Conflict(_) => CalmError::Conflict(
            "The Track changed. Check the original task key before starting another task.".into(),
        ),
        error => error,
    })?;
    let blocks = doc
        .blocks_snapshot()
        .map_err(|error| CalmError::Internal(format!("read task declarations: {error}")))?;
    let declared = blocks.iter().any(|block| {
        block.kind == report_blocks::KIND_TASK
            && block.payload.get("key") == Some(&serde_json::Value::String(key.to_string()))
    });
    let used: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_attempt_allocations WHERE track_id=?1 AND key=?2 \
         UNION ALL SELECT 1 FROM tasks WHERE track_id=?1 AND key=?2)",
    )
    .bind(track_id.as_str())
    .bind(key)
    .fetch_one(&mut **tx)
    .await?;
    if declared || used {
        return Err(CalmError::Conflict(
            "This task key already exists. Check its status and history.".into(),
        ));
    }
    let content = report_blocks::render_data_block(
        report_blocks::KIND_TASK,
        &serde_json::json!({
            "key": key, "kind": "codex", "goal": goal,
            "ready": true, "declared_by": "user",
            "no_gate_reason": "Independent task accepted through its completion report.",
            "context": {"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}}
        }),
    )
    .map_err(CalmError::BadRequest)?;
    Ok(ReportDocOp::UpsertBlock {
        id: None,
        kind: report_blocks::KIND_TASK.into(),
        content,
        if_rev: None,
        if_doc_rev: Some(if_doc_rev),
        position: None,
    })
}
