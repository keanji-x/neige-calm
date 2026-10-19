//! Compatibility reader for already-admitted operation records. New admission never uses it.
use calm_types::card_presentation::CardCreationSource;
use serde_json::Value;

use super::TerminalCreateOperationPayload;
use crate::error::Result;

pub(super) fn decode_stored(
    input: &Value,
    operation_kind: &str,
) -> Result<TerminalCreateOperationPayload> {
    if input.get("source").is_some() {
        return Ok(serde_json::from_value(input.clone())?);
    }
    // Normalize only this local copy. The original persisted request remains the claim CAS oracle.
    let mut normalized = input.clone();
    let hooks: Option<bool> = serde_json::from_value(
        normalized
            .get("planner_hooks")
            .cloned()
            .unwrap_or(Value::Null),
    )?;
    let source = match (operation_kind, hooks) {
        ("terminal-execution-create", _) => CardCreationSource::Manual,
        (_, Some(true)) => CardCreationSource::Agent,
        (_, Some(false)) => CardCreationSource::Manual,
        (_, None) => CardCreationSource::Unrecorded,
    };
    if let Some(fields) = normalized.as_object_mut() {
        fields.insert("source".into(), serde_json::to_value(source)?);
        fields.insert("planner_hooks".into(), Value::Bool(hooks == Some(true)));
    }
    Ok(serde_json::from_value(normalized)?)
}
