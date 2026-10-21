use super::*;
use calm_types::report_blocks::TASK_FIELDS;
use std::collections::BTreeSet;

fn task_schema(table: &Value) -> &Value {
    &table["kinds"]
        .as_array()
        .expect("kinds array")
        .iter()
        .find(|kind| kind["kind"] == "task")
        .expect("task kind table entry")["schema"]
}

fn assert_required_fields(path: &str, value: &Value, schema: &Value) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{path}: expected object, got {value}"));
    for field in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|field| field.as_str().expect("required field name"))
    {
        assert!(
            object.contains_key(field),
            "{path}: missing required {field}"
        );
    }
}

fn assert_value_matches_published_schema(path: &str, value: &Value, schema: &Value) {
    match schema["type"].as_str() {
        Some("object") => {
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{path}: expected object, got {value}"));
            assert_required_fields(path, value, schema);
            let properties = schema["properties"]
                .as_object()
                .expect("published object schema properties");
            for (field, child) in object {
                let child_schema = properties
                    .get(field)
                    .unwrap_or_else(|| panic!("{path}.{field}: field is not published"));
                assert_value_matches_published_schema(
                    &format!("{path}.{field}"),
                    child,
                    child_schema,
                );
            }
        }
        Some("array") => {
            let array = value
                .as_array()
                .unwrap_or_else(|| panic!("{path}: expected array, got {value}"));
            for (index, child) in array.iter().enumerate() {
                assert_value_matches_published_schema(
                    &format!("{path}[{index}]"),
                    child,
                    &schema["items"],
                );
            }
        }
        Some("string") => assert!(value.is_string(), "{path}: expected string, got {value}"),
        Some("integer") => assert!(value.is_i64(), "{path}: expected integer, got {value}"),
        Some("boolean") => assert!(value.is_boolean(), "{path}: expected boolean, got {value}"),
        other => panic!("{path}: unsupported published schema type {other:?}"),
    }
}

#[test]
fn task_is_advertised_by_the_commit_contract() {
    let table = kinds_table();
    let task = table["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "task")
        .expect("task kind table entry");
    assert_eq!(task["schema"]["additionalProperties"], false);
    assert_eq!(
        task["schema"]["properties"]["declared_by"]["enum"],
        json!(["spec", "user"])
    );
    let properties = &task["schema"]["properties"];
    assert_eq!(properties["acceptance"]["minLength"], 1);
    for field in ["goal", "command", "acceptance", "no_gate_reason"] {
        assert_eq!(properties[field]["pattern"], "\\S");
    }
    let goal_description = properties["goal"]["description"]
        .as_str()
        .expect("task goal description");
    assert!(
        goal_description.contains("Natural-language objective")
            && goal_description.contains("forbidden for terminal"),
        "task goal description must stay agent-only: {goal_description}"
    );
    let command_description = properties["command"]["description"]
        .as_str()
        .expect("task command description");
    assert!(
        command_description.contains("passed verbatim")
            && command_description.contains("Required only for terminal")
            && command_description.contains("forbidden for codex/claude"),
        "task command description must stay terminal-only: {command_description}"
    );
    assert_eq!(properties["priority"]["minimum"], i64::MIN);
    assert_eq!(properties["priority"]["maximum"], i64::MAX);
    for field in ["cwd", "gate"] {
        let cwd = if field == "gate" {
            &properties[field]["properties"]["cwd"]
        } else {
            &properties[field]
        };
        assert!(cwd["pattern"].as_str().unwrap().contains("\\x00-\\x1F"));
        assert!(!cwd["pattern"].as_str().unwrap().starts_with("^/"));
    }
    for field in ["name", "cmd"] {
        assert!(
            properties["gate"]["properties"]["steps"]["items"]["properties"][field]["pattern"]
                .as_str()
                .unwrap()
                .contains("\\x00-\\x1F")
        );
    }
    let usage = task["usage"].as_str().unwrap();
    assert!(usage.contains("ready: true"));
    assert!(
        usage.contains("terminal") && usage.contains("mutually exclusive"),
        "task usage must keep discriminated instruction fields visible: {usage}"
    );

    let commit = commit_descriptor();
    let commit_kinds =
        &commit.input_schema["properties"]["ops"]["items"]["properties"]["kind"]["enum"];
    assert!(
        commit_kinds
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "task")
    );
    let table_kinds: Vec<Value> = table["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].clone())
        .collect();
    assert_eq!(commit_kinds, &Value::Array(table_kinds));
}

#[test]
fn task_schema_properties_equal_validator_field_vocabulary() {
    let table = kinds_table();
    let published: BTreeSet<&str> = task_schema(&table)["properties"]
        .as_object()
        .expect("task schema properties")
        .keys()
        .map(String::as_str)
        .collect();
    let validator: BTreeSet<&str> = TASK_FIELDS.iter().copied().collect();
    assert_eq!(published, validator);
}

/// #2599: the published `result_contract` choices and the `task_result_contracts` section are
/// read off the kernel registry, so adding a contract edits no list here.
#[test]
fn published_result_contract_choices_equal_the_registry() {
    let table = kinds_table();
    let schema = task_schema(&table);
    let names: Vec<&str> = RESULT_CONTRACTS
        .iter()
        .map(|contract| contract.name)
        .collect();
    assert_eq!(
        schema["properties"]["result_contract"]["enum"],
        json!(names)
    );
    let tombstone_forbids: Vec<&Value> = schema["oneOf"][2]["not"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|clause| &clause["required"][0])
        .collect();
    assert!(tombstone_forbids.contains(&&json!("result_contract")));
    let published = table["task_result_contracts"].as_array().expect("section");
    assert_eq!(published.len(), names.len());
    for (entry, contract) in published.iter().zip(RESULT_CONTRACTS) {
        assert_eq!(entry["name"], contract.name);
        assert_eq!(entry["shape"], contract.shape_hint);
        assert_eq!(entry["schema"], (contract.schema)());
    }
}

/// The one gate-bearing task payload checked against the published schema (no builtin template file carries a `gate`).
#[test]
fn minimal_gate_task_payload_matches_published_task_schema_field_by_field() {
    let payload = json!({
        "key": "minimal-gate",
        "kind": "codex",
        "goal": "exercise the published gate wire shape",
        "depends_on": [],
        "gate": { "steps": [{ "name": "minimal", "cmd": "true" }] },
        "ready": true,
        "declared_by": "spec",
    });
    let table = kinds_table();
    let schema = task_schema(&table);

    assert_required_fields("task", &payload, &schema["oneOf"][0]);
    assert_value_matches_published_schema("task", &payload, schema);
}

#[test]
fn compact_task_kind_published_contract_matches_validator() {
    let table = kinds_table();
    let schema = task_schema(&table);
    let minimal = json!({"key":"compact", "kind":"compact", "ready":true,"declared_by":"spec"});
    let check = |value: &Value, expected| {
        assert_eq!(
            super::schema_checks::accepts(schema, value, schema),
            expected,
            "schema: {value}"
        );
        assert_eq!(
            calm_types::report_blocks::validate_payload("task", value).is_ok(),
            expected,
            "validator: {value}"
        );
    };
    check(&minimal, true);
    for field in TASK_FIELDS.iter().filter(|field| {
        !["key", "kind", "ready", "released_by_user", "declared_by"].contains(field)
    }) {
        let mut payload = minimal.clone();
        payload[*field] = Value::Null;
        check(&payload, false);
    }
    for (field, value, expected) in [
        ("declared_by", json!("user"), false),
        ("released_by_user", json!(true), true),
        ("released_by_user", Value::Null, false),
        ("ready", json!(false), true),
        ("ready", Value::Null, false),
        ("unknown", json!(true), false),
    ] {
        let mut payload = minimal.clone();
        payload[field] = value;
        check(&payload, expected);
    }
    for field in ["key", "kind", "ready", "declared_by"] {
        let mut payload = minimal.clone();
        payload.as_object_mut().unwrap().remove(field);
        check(&payload, false);
    }
}
