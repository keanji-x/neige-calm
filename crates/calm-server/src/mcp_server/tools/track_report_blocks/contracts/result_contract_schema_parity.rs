//! #2599: the published task schema and the kernel's declaration validator agree on every
//! `result_contract` declaration rule: a registered name, kind codex or claude, never a
//! child-track spawn, and never on a tombstone.

use super::*;
use calm_types::report_blocks::validate_payload;
use calm_types::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};

use super::schema_checks::accepts;

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

fn task_schema(table: &Value) -> &Value {
    &table["kinds"]
        .as_array()
        .expect("kinds array")
        .iter()
        .find(|kind| kind["kind"] == "task")
        .expect("task kind table entry")["schema"]
}

#[test]
fn published_task_schema_and_validator_agree_on_every_result_contract_declaration() {
    let table = kinds_table();
    let schema = task_schema(&table);
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

#[test]
fn child_track_config_published_schema_matches_validation() {
    let table = kinds_table();
    let schema = task_schema(&table);
    for (spawn, config, allowed) in [
        (
            "sub-track",
            json!({"provider":"claude", "model":"sonnet", "reasoning_effort":"high"}),
            true,
        ),
        ("sub-track", json!({}), true),
        (TASK_IN_TRACK_ROUTE, json!({}), false),
        ("sub-track", json!({"permission_mode":"full"}), false),
        ("sub-track", json!({"other":"unknown"}), false),
        ("sub-track", json!({"model":null}), false),
        ("sub-track", json!({"provider":" \t\n"}), false),
        ("sub-track", json!({"model":" \t\n"}), false),
        ("sub-track", json!({"reasoning_effort":" \t\n"}), false),
        ("sub-track", json!(null), false),
        ("sub-track", json!(false), false),
    ] {
        let mut task = agent("codex");
        task["spawn"] = json!(spawn);
        task["child_track"] = config;
        assert_eq!(accepts(schema, &task, schema), allowed, "schema: {task}");
        assert_eq!(
            validate_payload("task", &task).is_ok(),
            allowed,
            "validator: {task}"
        );
    }
}
