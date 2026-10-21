//! Strict report representation of the engine-owned objective definition.
use std::collections::BTreeMap;

use objective::definition::ObjectiveDefinition;
use serde_json::Value;

use super::{KIND_OBJECTIVE, canonical_json, kinds, tasks::key_is_valid};
use crate::track_report::ReportBlock;

pub fn parse_objective(payload: &Value) -> Result<ObjectiveDefinition, String> {
    let definition: ObjectiveDefinition =
        serde_path_to_error::deserialize(payload).map_err(|error| format!("objective: {error}"))?;
    let round_trip = serde_json::to_value(&definition).map_err(|error| error.to_string())?;
    if canonical_json(payload) != canonical_json(&round_trip) {
        return Err(format!(
            "{}: payload must equal its canonical definition (no unknown, defaulted or positional fields)",
            first_difference("objective", payload, &round_trip)
        ));
    }
    if !key_is_valid(definition.key.as_str()) {
        return Err("key: must match the task key grammar".into());
    }
    let mut errors = Vec::new();
    kinds::check_nested_string_caps("objective", payload, &mut errors);
    for diagnostic in definition.validate() {
        let problem =
            serde_json::to_string(&diagnostic.problem).map_err(|error| error.to_string())?;
        errors.push(format!("{}: {problem}", diagnostic.path));
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(definition)
}

fn first_difference(path: &str, left: &Value, right: &Value) -> String {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            for key in left.keys().chain(right.keys()) {
                let child = format!("{path}.{key}");
                match (left.get(key), right.get(key)) {
                    (Some(a), Some(b)) if canonical_json(a) != canonical_json(b) => {
                        return first_difference(&child, a, b);
                    }
                    (None, _) | (_, None) => return child,
                    _ => {}
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            for (index, (a, b)) in left.iter().zip(right).enumerate() {
                if canonical_json(a) != canonical_json(b) {
                    return first_difference(&format!("{path}[{index}]"), a, b);
                }
            }
        }
        _ => {}
    }
    path.into()
}

/// The one parser for document-local definitions; keys cannot alias even across block ids.
pub fn objective_blocks(
    blocks: &[ReportBlock],
) -> Result<BTreeMap<String, (&ReportBlock, ObjectiveDefinition)>, String> {
    let mut out = BTreeMap::new();
    for block in blocks.iter().filter(|block| block.kind == KIND_OBJECTIVE) {
        kinds::validate_payload(KIND_OBJECTIVE, &block.payload)?;
        let definition = parse_objective(&block.payload)?;
        let key = definition.key.as_str().to_owned();
        if out.insert(key.clone(), (block, definition)).is_some() {
            return Err(format!("duplicate objective key `{key}`"));
        }
    }
    Ok(out)
}

/// Discovery schema. Semantic diagnostics and canonical round-trip requirements also apply.
pub fn schema() -> Value {
    serde_json::from_str(include_str!("objective.schema.json")).expect("objective discovery schema")
}
