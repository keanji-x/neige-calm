//! One explicitly selected task, one private controller, one existing Operation.
pub mod adapter;
mod admission;
pub mod config;
mod journal;
pub(crate) mod lookup;
pub use lookup::private_codex_home;
mod observe;
mod record;
pub(crate) mod turn;
mod workspace;

use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::model::Task;
use calm_types::task_execution::IsolatedCodexSelection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPERATION_KIND: &str = "codex-isolated-worker";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerVersion {
    #[serde(rename = "isolated-worker-v1")]
    V1,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerPayload {
    pub version: WorkerVersion,
    pub actor: ActorId,
    pub track_id: String,
    pub task_id: String,
    pub idempotency_key: String,
}

pub fn selected(task: &Task) -> Result<bool> {
    let context: Value = serde_json::from_str(&task.context_json)?;
    let Some(selection) =
        IsolatedCodexSelection::from_context(&context).map_err(CalmError::BadRequest)?
    else {
        return Ok(false);
    };
    let dependencies: Vec<String> = serde_json::from_str(&task.depends_on_json)?;
    let kind = serde_json::to_value(task.kind)?;
    selection
        .validate_route(
            kind.as_str().unwrap_or(""),
            &task.spawn,
            !dependencies.is_empty(),
            task.gate_json.is_some(),
        )
        .map_err(CalmError::BadRequest)?;
    Ok(true)
}
pub fn worker_payload(task: &Task) -> WorkerPayload {
    WorkerPayload {
        version: WorkerVersion::V1,
        actor: ActorId::KernelDispatcher,
        track_id: task.track_id.clone(),
        task_id: task.id.clone(),
        idempotency_key: task.id.clone(),
    }
}

#[cfg(test)]
mod checkpoint_tests;
