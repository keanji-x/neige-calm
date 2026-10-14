//! #2599: the published task schema and the kernel's declaration validator agree on every
//! `result_contract` declaration rule: a registered name, kind codex or claude, never a
//! child-track spawn, and never on a tombstone.

use super::*;
use calm_types::report_blocks::validate_payload;
use calm_types::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};

/// Evaluates the JSON Schema keywords the published task schema uses. A keyword outside that set
/// panics instead of passing, so the evaluator cannot silently accept what it does not check.
fn accepts(schema: &Value, value: &Value, root: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return schema
            .as_bool()
            .expect("a schema is an object or a boolean");
    };
    schema.iter().all(|(keyword, rule)| match keyword.as_str() {
        "description" | "default" | "examples" | "title" | "$defs" | "$schema" => true,
        "$ref" => {
            let name = rule
                .as_str()
                .and_then(|path| path.strip_prefix("#/$defs/"))
                .expect("a local $defs reference");
            accepts(&root["$defs"][name], value, root)
        }
        "type" => match rule {
            Value::Array(types) => types.iter().any(|kind| is_type(kind, value)),
            kind => is_type(kind, value),
        },
        "enum" => rule.as_array().expect("enum array").contains(value),
        "const" => rule == value,
        "not" => !accepts(rule, value, root),
        "anyOf" => branches(rule).any(|branch| accepts(branch, value, root)),
        "allOf" => branches(rule).all(|branch| accepts(branch, value, root)),
        "oneOf" => branches(rule).filter(|b| accepts(b, value, root)).count() == 1,
        "required" => value.as_object().is_none_or(|object| {
            rule.as_array()
                .expect("required array")
                .iter()
                .all(|field| object.contains_key(field.as_str().expect("field name")))
        }),
        "properties" => value.as_object().is_none_or(|object| {
            object.iter().all(|(field, child)| {
                rule.get(field)
                    .is_none_or(|schema| accepts(schema, child, root))
            })
        }),
        "additionalProperties" => value.as_object().is_none_or(|object| {
            let declared = schema.get("properties").and_then(Value::as_object);
            object
                .iter()
                .filter(|(field, _)| declared.is_none_or(|declared| !declared.contains_key(*field)))
                .all(|(_, child)| accepts(rule, child, root))
        }),
        "items" => value
            .as_array()
            .is_none_or(|items| items.iter().all(|item| accepts(rule, item, root))),
        "minLength" | "maxLength" | "pattern" => value.as_str().is_none_or(|text| {
            let chars = text.chars().count() as u64;
            match keyword.as_str() {
                "minLength" => chars >= rule.as_u64().expect("minLength"),
                "maxLength" => chars <= rule.as_u64().expect("maxLength"),
                _ => regex::Regex::new(rule.as_str().expect("pattern"))
                    .expect("a pattern the evaluator can compile")
                    .is_match(text),
            }
        }),
        other => panic!("the parity evaluator does not implement keyword {other:?}"),
    })
}

fn branches(rule: &Value) -> impl Iterator<Item = &Value> {
    rule.as_array().expect("combinator array").iter()
}

fn is_type(kind: &Value, value: &Value) -> bool {
    match kind.as_str().expect("type name") {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        other => panic!("unknown JSON Schema type {other:?}"),
    }
}

fn agent(kind: &str) -> Value {
    json!({"key": "review", "kind": kind, "goal": "review the diff", "ready": true,
        "declared_by": "user"})
}

fn with(mut task: Value, field: &str, value: Value) -> Value {
    task[field] = value;
    task
}

/// Every allowed and forbidden `result_contract` declaration, with the verdict both the published
/// schema and `validate_payload` must reach. Allowed rows iterate the registry.
fn declarations() -> Vec<(String, Value, bool)> {
    let terminal = json!({"key": "build", "kind": "terminal", "command": "true", "ready": true,
        "declared_by": "user"});
    let tombstone = json!({"key": "review", "tombstone": {"reason": "dropped"},
        "declared_by": "user", "tombstoned_by": "user"});
    let mut cases = vec![
        (
            format!("codex {TASK_CHILD_TRACK_ROUTE} without a contract"),
            with(agent("codex"), "spawn", json!(TASK_CHILD_TRACK_ROUTE)),
            true,
        ),
        ("terminal without a contract".into(), terminal.clone(), true),
        (
            "tombstone without a contract".into(),
            tombstone.clone(),
            true,
        ),
        (
            "unregistered name".into(),
            with(agent("codex"), "result_contract", json!("review-v0")),
            false,
        ),
        (
            "non-string name".into(),
            with(agent("claude"), "result_contract", json!(1)),
            false,
        ),
    ];
    for contract in RESULT_CONTRACTS {
        let name = json!(contract.name);
        for kind in ["codex", "claude"] {
            let declared = with(agent(kind), "result_contract", name.clone());
            cases.extend([
                (format!("{kind} {}", contract.name), declared.clone(), true),
                (
                    format!("{kind} {} {TASK_IN_TRACK_ROUTE}", contract.name),
                    with(declared.clone(), "spawn", json!(TASK_IN_TRACK_ROUTE)),
                    true,
                ),
                (
                    format!("{kind} {} read_only", contract.name),
                    with(declared.clone(), "access", json!("read_only")),
                    true,
                ),
                (
                    format!("{kind} {} {TASK_CHILD_TRACK_ROUTE}", contract.name),
                    with(declared, "spawn", json!(TASK_CHILD_TRACK_ROUTE)),
                    false,
                ),
            ]);
        }
        cases.extend([
            (
                format!("terminal {}", contract.name),
                with(terminal.clone(), "result_contract", name.clone()),
                false,
            ),
            (
                format!("tombstone {}", contract.name),
                with(tombstone.clone(), "result_contract", name),
                false,
            ),
        ]);
    }
    cases
}

#[test]
fn published_task_schema_and_validator_agree_on_every_result_contract_declaration() {
    let table = kinds_table();
    let schema = &table["kinds"]
        .as_array()
        .expect("kinds array")
        .iter()
        .find(|kind| kind["kind"] == "task")
        .expect("task kind table entry")["schema"];
    for (case, payload, allowed) in declarations() {
        assert_eq!(
            accepts(schema, &payload, schema),
            allowed,
            "{case}: published schema verdict, payload {payload}"
        );
        assert_eq!(
            validate_payload("task", &payload).is_ok(),
            allowed,
            "{case}: validator verdict {:?}, payload {payload}",
            validate_payload("task", &payload)
        );
    }
}
