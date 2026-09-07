//! Snapshot file authority from retained Operations, then read through pinned FDs.
use crate::error::{CalmError, Result};
use crate::model::TaskStatus;
use crate::operation::Tx;
use crate::routes::fs::{WorkspaceSymlinks, open_workspace_regular_file_at};
use std::{fs::File, os::unix::fs::MetadataExt, path::PathBuf};
use tokio::io::AsyncReadExt;

pub(crate) const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct FileSnapshot {
    operation_id: String,
    workspace: PathBuf,
}

fn stop_unavailable() -> CalmError {
    CalmError::Conflict("Task files require a completed execution with a confirmed stop.".into())
}
fn missing_file() -> CalmError {
    CalmError::NotFound("Reported task file is unavailable or not a regular file.".into())
}

pub(crate) async fn snapshot_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    key: &str,
    attempt_id: &str,
) -> Result<FileSnapshot> {
    let task = crate::db::sqlite::task_get_tx(tx, attempt_id)
        .await?
        .filter(|task| task.track_id == track_id && task.key == key)
        .ok_or_else(stop_unavailable)?;
    if task.status != TaskStatus::Done
        || !super::selected(&task).map_err(|_| stop_unavailable())?
        || task.gate_attempt != 0
        || task.gate_pid.is_some()
        || task.gate_result_json.is_some()
    {
        return Err(stop_unavailable());
    }
    let operations: Vec<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id,kind,phase,spawn_artifacts_json,compensation_state FROM operations \
         WHERE idempotency_key=?1 OR (kind='task-verify' AND json_extract(payload_json,'$.task_id')=?1)")
        .bind(attempt_id).fetch_all(&mut **tx).await?;
    let [(id, kind, phase, spawn, compensation)] = operations.as_slice() else {
        return Err(stop_unavailable());
    };
    if kind != super::OPERATION_KIND
        || phase != "succeeded"
        || spawn.is_some()
        || compensation.is_some()
    {
        return Err(stop_unavailable());
    }
    let record = super::recovery::confirmed_record_tx(tx, &task, id)
        .await
        .map_err(|_| stop_unavailable())?;
    Ok(FileSnapshot {
        operation_id: id.clone(),
        workspace: record.request.workspace,
    })
}

pub(crate) fn relative_reference(reference: &str) -> Result<String> {
    let relative = reference.strip_prefix("/workspace/").unwrap_or(reference);
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains(['\\', ':'])
        || relative.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".." | ".codex"))
    {
        return Err(CalmError::BadRequest(
            "Unsupported reported task file reference.".into(),
        ));
    }
    Ok(relative.to_string())
}

impl FileSnapshot {
    pub(crate) fn open(self) -> Result<File> {
        // Root authority comes from the validated immutable original request,
        // not a currently enabled provider or the Track's mutable workspace.
        let root = self.workspace.parent().ok_or_else(|| {
            CalmError::Conflict("Task workspace ownership is unavailable.".into())
        })?;
        super::workspace::open_retained(root, &self.operation_id, &self.workspace)
    }
}

/// Consumes the validated descriptor, never a root pathname. Keeping this read
/// stage separate makes directory replacement after validation harmless.
pub(crate) async fn read(directory: File, relative: &str) -> Result<Vec<u8>> {
    let opened = open_workspace_regular_file_at(directory, relative, WorkspaceSymlinks::Refused)
        .await
        .map_err(|_| missing_file())?;
    let meta = opened.file.metadata().await.map_err(|_| missing_file())?;
    if meta.nlink() != 1 {
        return Err(missing_file());
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(CalmError::PayloadTooLarge(
            "Reported task file exceeds 8 MiB.".into(),
        ));
    }
    let mut bytes = Vec::new();
    opened
        .file
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| missing_file())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(CalmError::PayloadTooLarge(
            "Reported task file exceeds 8 MiB.".into(),
        ));
    }
    Ok(bytes)
}
