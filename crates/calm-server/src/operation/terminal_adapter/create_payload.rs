//! Compatibility reader for already-admitted operation records. New admission never uses it.
use super::{TerminalCreateOperationPayload, TerminalCreateRequestPayload};
use crate::error::Result;
use crate::ids::ActorId;
use calm_types::card_presentation::CardCreationSource;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct LegacyTerminalCreate {
    actor: ActorId,
    #[serde(default, rename = "runtime_id")]
    worker_session_id: Option<String>,
    planner_hooks: Option<bool>,
    #[serde(flatten)]
    request: TerminalCreateRequestPayload,
}

pub(super) fn decode_stored(
    input: &Value,
    operation_kind: &str,
) -> Result<TerminalCreateOperationPayload> {
    if input.get("source").is_some() {
        return Ok(serde_json::from_value(input.clone())?);
    }
    let legacy: LegacyTerminalCreate = serde_json::from_value(input.clone())?;
    let source = match (operation_kind, legacy.planner_hooks) {
        (_, Some(true)) => CardCreationSource::Agent,
        (_, Some(false)) => CardCreationSource::Manual,
        ("terminal-execution-create", None) => CardCreationSource::Manual,
        (_, None) => CardCreationSource::Unrecorded,
    };
    Ok(TerminalCreateOperationPayload {
        source,
        actor: legacy.actor,
        worker_session_id: legacy.worker_session_id,
        planner_hooks: legacy.planner_hooks == Some(true),
        request: legacy.request,
    })
}
