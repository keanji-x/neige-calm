//! Task payload shape validation behind the shared kind dispatcher.
use super::*;

pub const TASK_FIELDS: &[&str] = &[
    "key",
    "kind",
    "goal",
    "command",
    "acceptance",
    "gate",
    "no_gate_reason",
    "depends_on",
    "priority",
    "cwd",
    "context",
    "refs",
    "ready",
    "declared_by",
    "released_by_user",
    "spawn",
    "access",
    "head",
    "base",
    "start",
    "result_contract",
    "child_track",
    "tombstone",
    "tombstoned_by",
];

pub(super) fn validate_task(map: &Map<String, Value>, errors: &mut Vec<String>) {
    reject_unknown(map, TASK_FIELDS, errors);

    let key = match map.get("key") {
        Some(Value::String(key)) if key_is_valid(key) => {
            check_string_cap("key", key, errors);
            Some(key.as_str())
        }
        Some(Value::String(_)) => {
            errors.push("key: must match ^[a-z0-9][a-z0-9._-]{0,63}$".into());
            None
        }
        _ => {
            errors.push("key: required string matching ^[a-z0-9][a-z0-9._-]{0,63}$".into());
            None
        }
    };

    let tombstone = map.get("tombstone").is_some_and(|value| !value.is_null());
    if tombstone {
        for field in TASK_FIELDS
            .iter()
            .filter(|field| !["key", "tombstone", "declared_by", "tombstoned_by"].contains(field))
        {
            if map.contains_key(*field) {
                errors.push(format!("{field}: must be absent from a tombstone task"));
            }
        }
        validate_declared_by(map, errors);
        required_enum(map, "tombstoned_by", &["spec", "user"], errors);
        match &map["tombstone"] {
            Value::Object(value) => {
                reject_unknown(value, &["reason"], errors);
                if let Some(reason) = value.get("reason") {
                    match reason {
                        Value::Null => {}
                        Value::String(reason) => {
                            check_string_cap("tombstone.reason", reason, errors)
                        }
                        _ => errors.push("tombstone.reason: must be a string or null".into()),
                    }
                }
            }
            _ => errors.push("tombstone: must be an object".into()),
        }
        return;
    }

    if map.contains_key("tombstoned_by") {
        errors.push("tombstoned_by: must be absent from a non-tombstone task".into());
    }
    let task_kind = match map.get("kind").and_then(Value::as_str) {
        Some(kind @ ("codex" | "claude" | "terminal" | "compact")) => Some(kind),
        _ => {
            errors.push(
                "kind: required; must be one of \"codex\" | \"claude\" | \"terminal\" | \"compact\"".into(),
            );
            None
        }
    };
    if task_kind == Some("compact") {
        reject_unknown(
            map,
            &["key", "kind", "ready", "released_by_user", "declared_by"],
            errors,
        );
        required_enum(
            map,
            "declared_by",
            &[crate::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR],
            errors,
        );
        if !matches!(map.get("ready"), Some(Value::Bool(_))) {
            errors.push("ready: required boolean".into());
        }
        if let Some(value) = map.get("released_by_user")
            && !value.is_boolean()
        {
            errors.push("released_by_user: must be a boolean".into());
        }
        return;
    }
    match task_kind {
        Some("terminal") => {
            required_non_empty_string(map, "command", errors);
            if map.contains_key("goal") {
                errors.push("goal: must be absent when kind is \"terminal\"".into());
            }
        }
        Some("codex" | "claude") => {
            required_non_empty_string(map, "goal", errors);
            if map.contains_key("command") {
                errors.push("command: must be absent when kind is \"codex\" or \"claude\"".into());
            }
        }
        _ => {}
    }
    optional_non_empty_string(map, "acceptance", errors);
    optional_non_empty_string(map, "no_gate_reason", errors);
    optional_abs_path(map, "cwd", errors);
    if let Some(gate) = map.get("gate") {
        match serde_json::from_value::<GateInput>(gate.clone()) {
            Ok(gate) => {
                if let Err(error) = validate_gate_shape(key.unwrap_or("<invalid>"), &gate) {
                    let detail = error
                        .strip_prefix(&format!("task {}: ", key.unwrap_or("<invalid>")))
                        .unwrap_or(error.as_str());
                    errors.push(detail.to_string());
                }
                check_gate_strings(&gate, errors);
            }
            Err(error) => errors.push(format!("gate: invalid shape: {error}")),
        }
    }
    optional_string_array(map, "depends_on", false, errors);
    if let Some(priority) = map.get("priority")
        && priority.as_i64().is_none()
    {
        errors.push(format!(
            "priority: must be an integer between {} and {}",
            i64::MIN,
            i64::MAX
        ));
    }
    if let Some(context) = map.get("context") {
        check_nested_string_caps("context", context, errors);
    }
    match map.get("refs") {
        None => {}
        Some(Value::Array(refs)) => {
            for (index, reference) in refs.iter().enumerate() {
                match reference.as_str() {
                    Some(reference) => {
                        check_string_cap(&format!("refs[{index}]"), reference, errors);
                        if !matches!(parse_destination(reference), Some((_, Some(_)))) {
                            errors.push(format!("refs[{index}]: must be a neige://wave/<track>#b_xxxx block reference"));
                        }
                    }
                    None => errors.push(format!("refs[{index}]: must be a string")),
                }
            }
        }
        Some(_) => errors.push("refs: must be an array".into()),
    }
    if !matches!(map.get("ready"), Some(Value::Bool(_))) {
        errors.push("ready: required boolean".into());
    }
    validate_declared_by(map, errors);
    if let Some(value) = map.get("released_by_user")
        && !value.is_boolean()
    {
        errors.push("released_by_user: must be a boolean".into());
    }
    if let Some(value) = map.get("spawn")
        && !value.is_null()
        && !matches!(value.as_str(), Some("in-wave" | "sub-track"))
    {
        errors.push("spawn: must be one of \"in-wave\" | \"sub-track\"".into());
    }
    if matches!(map.get("spawn").and_then(Value::as_str), Some("sub-track"))
        && !matches!(map.get("kind").and_then(Value::as_str), Some("codex"))
    {
        errors.push("spawn: \"sub-track\" requires kind \"codex\"".into());
    }
    if let Some(value) = map.get("child_track") {
        if map.get("spawn").and_then(Value::as_str) != Some("sub-track") {
            errors.push("child_track: requires spawn \"sub-track\"".into());
        }
        match serde_json::from_value::<crate::report_blocks::tasks::ChildTrackConfig>(value.clone())
        {
            Ok(config) => {
                if let Some(object) = value.as_object() {
                    for (name, member) in object {
                        if !member.is_string() {
                            errors.push(format!("child_track.{name}: must be a string"));
                        }
                    }
                }
                for (name, value) in [
                    ("provider", config.provider),
                    ("model", config.model),
                    ("reasoning_effort", config.reasoning_effort),
                ] {
                    if let Some(value) = value {
                        if value.trim().is_empty() {
                            errors.push(format!("child_track.{name}: must be non-empty"));
                        }
                        check_string_cap(&format!("child_track.{name}"), &value, errors);
                    }
                }
                if !value.is_object() {
                    errors.push("child_track: must be an object".into());
                }
            }
            Err(error) => errors.push(format!("child_track: invalid shape: {error}")),
        }
    }
    crate::task_execution::validate_task_access(map, errors);
    crate::task_execution::validate_reader_commits(map, errors);
    crate::task_execution::validate_task_start(map, errors);
    crate::task_result::validate_task_result_contract(map, errors);
}
