//! One isolated producer, one sealed JSON document, one claim-bound consumer.
pub mod adapter;
pub(crate) mod candidate;
pub(crate) mod candidate_input;
pub(crate) mod candidate_verify;
mod candidate_view;
mod input;
mod publication;
pub(crate) mod settlement;
pub(crate) mod verification_settlement;
mod view;
use crate::{
    error::{CalmError, Result},
    model::Task,
};
use calm_task_artifacts::{ArtifactStore, Limits};
use calm_types::task_execution::{FileDelivery, IsolatedCodexSelection};
pub(crate) use input::{
    bind_claim_tx, prepare_input, prompt_tx, require_recovery_input_tx, verify_input,
};
pub(crate) use publication::{PublicationPayload, source_tx};
use std::path::Path;
pub(crate) use view::view_tx;

pub const OPERATION_KIND: &str = "task-file-publication";
pub(crate) fn selection(task: &Task) -> Result<Option<FileDelivery>> {
    let context: serde_json::Value = serde_json::from_str(&task.context_json)?;
    // This protocol only owns explicit file_delivery declarations. Historical
    // contexts without that field keep their established execution semantics.
    if context
        .get("neige_execution")
        .and_then(|value| value.get("file_delivery"))
        .is_none()
    {
        return Ok(None);
    }
    let selected = IsolatedCodexSelection::from_context(&context).map_err(CalmError::BadRequest)?;
    if let Some(selected) = &selected {
        selected
            .validate_delivery()
            .map_err(CalmError::BadRequest)?;
    }
    Ok(selected.and_then(|selection| selection.file_delivery))
}
pub(crate) fn conflict(reason: impl Into<String>) -> CalmError {
    CalmError::Conflict(reason.into())
}
pub(crate) fn artifact_error(error: calm_task_artifacts::Error) -> CalmError {
    let detail = match error {
        calm_task_artifacts::Error::Io(_) => "source or sealed storage is unavailable",
        calm_task_artifacts::Error::Json(_) => "sealed manifest is invalid",
        calm_task_artifacts::Error::Invalid(_) => "invalid file source or request",
        calm_task_artifacts::Error::Unsupported(_) => "unsupported file source",
        calm_task_artifacts::Error::Limit(_) => "file exceeds delivery limits",
        calm_task_artifacts::Error::Integrity(_) => "sealed input integrity mismatch",
        calm_task_artifacts::Error::Conflict => "capture key binds a different output",
        calm_task_artifacts::Error::MissingOutput { .. } => "declared output is missing",
        calm_task_artifacts::Error::DestinationExists(_) => {
            "input destination conflicts with the frozen binding"
        }
    };
    conflict(format!("file delivery: {detail}"))
}
pub(crate) fn limits() -> Limits {
    Limits {
        max_entries: 64,
        max_file_bytes: 8 * 1024 * 1024,
        max_total_bytes: 8 * 1024 * 1024,
        max_manifest_bytes: 64 * 1024,
        max_path_bytes: 2048,
        max_depth: 64,
    }
}
pub(crate) fn store(root: &Path) -> Result<ArtifactStore> {
    ArtifactStore::open_files(root, limits()).map_err(artifact_error)
}

#[cfg(any(test, feature = "fixtures"))]
mod test_hooks;
#[cfg(any(test, feature = "fixtures"))]
pub use test_hooks::{CandidateReleaseHook, install_candidate_release_hook};
