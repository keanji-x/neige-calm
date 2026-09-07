use super::*;
use crate::{
    db::sqlite::{task_attempt_current_tx, task_get_tx},
    model::TaskStatus,
    operation::{Operation, Tx},
};
use calm_task_artifacts::{Entry, FileArtifactPath, FileCaptureRequest, SnapshotId};
use serde::{Deserialize, Serialize};
use std::{fs::File, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationPayload {
    pub task_id: String,
    pub track_id: String,
    pub source_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub publication_operation_id: String,
    pub source: PublicationPayload,
    pub contract: FileDelivery,
    pub snapshot: SnapshotId,
    pub file_digest: calm_task_artifacts::Digest,
    pub store_root: PathBuf,
}

pub(crate) async fn source_tx(
    tx: &mut Tx<'_>,
    payload: &PublicationPayload,
) -> Result<(Task, crate::isolated_codex::files::FileSnapshot)> {
    let task = task_get_tx(tx, &payload.task_id)
        .await?
        .ok_or_else(|| conflict("file producer is missing"))?;
    let current = task_attempt_current_tx(tx, &task.track_id, &task.key)
        .await?
        .ok_or_else(|| conflict("file producer allocation is missing"))?;
    if current.attempt_id != task.id
        || task.track_id != payload.track_id
        || task.status != TaskStatus::Done
        || !matches!(selection(&task)?, Some(FileDelivery::Producer { .. }))
    {
        return Err(conflict("file producer is obsolete or not completed"));
    }
    crate::task_recovery::validate_frozen_contract_tx(tx, &task).await?;
    if !matches!(
        crate::routes::isolated_tasks::accepted_report_tx(tx, &task.track_id, &task.key, &task.id)
            .await?,
        Some(crate::routes::isolated_tasks::AcceptedTaskReport::Completed { .. })
    ) {
        return Err(conflict("file producer has no accepted completion report"));
    }
    let source =
        crate::isolated_codex::files::snapshot_tx(tx, &task.track_id, &task.key, &task.id).await?;
    if source.operation_id() != payload.source_operation_id {
        return Err(conflict("file producer operation changed"));
    }
    Ok((task, source))
}

pub(super) async fn authorize_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
) -> Result<(Task, crate::isolated_codex::files::FileSnapshot)> {
    let payload: PublicationPayload = serde_json::from_value(op.payload.clone())?;
    if op.kind != OPERATION_KIND
        || op.idempotency_key.as_deref() != Some(&format!("file:{}", payload.task_id))
    {
        return Err(conflict("file publication operation identity changed"));
    }
    source_tx(tx, &payload).await
}

pub(super) fn capture(
    op: Operation,
    task: Task,
    source: crate::isolated_codex::files::FileSnapshot,
) -> Result<Receipt> {
    let contract = selection(&task)?.ok_or_else(|| conflict("output contract missing"))?;
    let FileDelivery::Producer { slot, path, .. } = &contract else {
        return Err(conflict("not a file producer"));
    };
    let root = source.store_root()?;
    let store = store(&root)?;
    let path = FileArtifactPath::new(path, &limits()).map_err(artifact_error)?;
    let payload: PublicationPayload = serde_json::from_value(op.payload.clone())?;
    let capture = store
        .capture_file(
            FileCaptureRequest {
                key: &op.id,
                boundary_id: &payload.source_operation_id,
                output: slot,
                path: &path,
            },
            || {
                let directory = source.open().map_err(|_| {
                    calm_task_artifacts::Error::Invalid(
                        "source workspace ownership unavailable".into(),
                    )
                })?;
                open_file(&directory, path.as_str())
            },
        )
        .map_err(artifact_error)?;
    // Policy applies to the sealed version, never a second mutable source read.
    let bytes = store
        .read_snapshot_file(&capture.snapshot, &path)
        .map_err(artifact_error)?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|e| conflict(format!("json-document-v1 verification failed: {e}")))?;
    let snapshot = store
        .open_snapshot(&capture.snapshot)
        .map_err(artifact_error)?;
    let digest = snapshot
        .manifest()
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::File {
                path: candidate,
                digest,
                ..
            } if candidate == path.as_str() => Some(digest.clone()),
            _ => None,
        })
        .ok_or_else(|| conflict("sealed file identity missing"))?;
    Ok(Receipt {
        publication_operation_id: op.id,
        source: payload,
        contract,
        snapshot: capture.snapshot,
        file_digest: digest,
        store_root: root,
    })
}
fn open_file(root: &File, path: &str) -> calm_task_artifacts::Result<File> {
    Ok(crate::routes::fs::open_workspace_regular_file_fd(
        root,
        std::path::Path::new(path),
        crate::routes::fs::WorkspaceSymlinks::Refused,
        true,
    )?)
}
