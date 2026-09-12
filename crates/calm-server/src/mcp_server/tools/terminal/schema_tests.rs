use crate::mcp_server::build_default_registry;
use crate::model::CardRole;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn fields(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
fn required(schema: &Value) -> BTreeSet<String> {
    schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn terminal_discovery_preserves_complete_typed_selector_and_action_arms() {
    let descriptors = build_default_registry().descriptors_for_role(CardRole::Planner);
    // Reproducible diagnostics for real JSON Schema validator checks; emitted
    // from the actual registration, not a hand-written schema or renderer copy.
    let terminal_schemas: serde_json::Map<String, Value> = descriptors
        .iter()
        .filter(|descriptor| descriptor.name.starts_with("calm.terminal."))
        .map(|descriptor| (descriptor.name.clone(), descriptor.input_schema.clone()))
        .collect();
    println!("TERMINAL_TOOL_SCHEMAS={}", Value::Object(terminal_schemas));
    assert_eq!(
        descriptors
            .iter()
            .filter(|descriptor| descriptor.name.starts_with("calm.terminal.")
                && descriptor.name != "calm.terminal.open")
            .map(|descriptor| descriptor.name.clone())
            .collect::<BTreeSet<_>>(),
        fields(&[
            "calm.terminal.resolve",
            "calm.terminal.observe",
            "calm.terminal.control",
            "calm.terminal.input"
        ]),
        "every registered targeted Terminal tool must be covered by this sweep"
    );
    for (name, common, mandatory) in [
        ("resolve", vec![], vec![]),
        (
            "observe",
            vec![
                "scroll_offset",
                "wait_ms",
                "wait_for",
                "settle_ms",
                "format",
            ],
            vec![],
        ),
        (
            "control",
            vec!["action", "observe", "wait_ms", "wait_for", "settle_ms"],
            vec!["action"],
        ),
        (
            "input",
            vec![
                "observation_id",
                "request_id",
                "action",
                "observe",
                "wait_ms",
                "wait_for",
                "settle_ms",
                "allow_output_since_observation",
            ],
            vec!["request_id", "action"],
        ),
    ] {
        let name = format!("calm.terminal.{name}");
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.name == name)
            .unwrap();
        let schema = &descriptor.input_schema;
        assert_eq!(
            schema["type"], "object",
            "{name}: MCP root remains an object"
        );
        assert_eq!(
            required(schema),
            fields(&mandatory),
            "{name}: root required fields"
        );
        let arms = schema["anyOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: discovery needs supported complete anyOf arms"));
        assert_eq!(arms.len(), 2, "{name}");
        for (arm, selector) in arms.iter().zip(["terminal_id", "task_id"]) {
            assert_eq!(arm["type"], "object", "{name}/{selector}");
            assert_eq!(arm["additionalProperties"], false, "{name}/{selector}");
            let mut expected_properties = fields(&common);
            expected_properties.insert(selector.into());
            assert_eq!(
                arm["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected_properties,
                "{name}/{selector}: all common properties, exactly one selector"
            );
            let mut expected_required = fields(&mandatory);
            expected_required.insert(selector.into());
            assert_eq!(
                required(arm),
                expected_required,
                "{name}/{selector}: a renderer must not lose common required fields"
            );
            for field in expected_properties {
                assert_eq!(
                    arm["properties"][&field], schema["properties"][&field],
                    "{name}/{selector}/{field}: retain complete field schema"
                );
            }
        }
        assert!(
            !schema.to_string().contains("\"oneOf\""),
            "{name}: local transformer does not model oneOf"
        );
        assert!(
            schema.to_string().len() < 4000,
            "{name}: avoid model schema compaction"
        );
        if name != "calm.terminal.resolve" {
            // #1618 waiting arguments: raised budget, change mode and settle.
            // The omitted-budget default depends on wait_for, so it is stated
            // in the description rather than as a single JSON Schema default.
            assert_eq!(
                schema["properties"]["wait_ms"],
                json!({"type":"integer","minimum":0,"maximum":20000,
                    "description":"Omitted: 0 for wait_for=elapsed, 2000 for wait_for=change"}),
                "{name}"
            );
            assert!(
                descriptor.description.contains("2000 for")
                    && descriptor.description.contains("change"),
                "{name}: tool description states the change-mode default budget"
            );
            assert_eq!(
                schema["properties"]["wait_for"],
                json!({"type":"string","enum":["elapsed","change"],"default":"elapsed"}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["settle_ms"],
                json!({"type":"integer","minimum":0,"maximum":2000,"default":150}),
                "{name}"
            );
        }
    }
    // #1618 round 07/08 guidance lives in the descriptions, not the schema.
    let description = |name: &str| {
        descriptors
            .iter()
            .find(|descriptor| descriptor.name == name)
            .unwrap()
            .description
            .clone()
    };
    let observe = description("calm.terminal.observe");
    assert!(
        observe.contains("baseline_revision") && observe.contains("previous_observation_revision")
    );
    let control = description("calm.terminal.control");
    assert!(control.contains("text_omitted") && control.contains("500 ms"));
    let input_description = description("calm.terminal.input");
    assert!(
        input_description.contains("stale_observation")
            && input_description.contains("never for menu selection or clicks")
            && input_description.contains("Omit observation_id")
    );
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "calm.terminal.input")
        .unwrap()
        .input_schema;
    assert_eq!(
        input["properties"]["allow_output_since_observation"],
        json!({"type":"boolean","default":false})
    );
    assert_eq!(
        input["properties"]["observation_id"],
        json!({"type":"string","format":"uuid"}),
        "observation_id stays typed while optional"
    );
    for arm in input["anyOf"].as_array().unwrap() {
        assert!(
            !required(arm).contains("observation_id"),
            "observation_id is optional in both arms"
        );
        assert!(arm["properties"].get("observation_id").is_some());
        assert!(
            arm["properties"]
                .get("allow_output_since_observation")
                .is_some()
        );
        let actions = arm["properties"]["action"]["anyOf"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        for (action, kind, properties, mandatory) in [
            (
                &actions[0],
                "text",
                vec!["type", "text"],
                vec!["type", "text"],
            ),
            (
                &actions[1],
                "key",
                vec!["type", "key", "repeat"],
                vec!["type", "key"],
            ),
            (
                &actions[2],
                "click",
                vec!["type", "column", "row"],
                vec!["type", "column", "row"],
            ),
        ] {
            assert_eq!(action["type"], "object");
            assert_eq!(action["additionalProperties"], false);
            assert_eq!(action["properties"]["type"], json!({"const":kind}));
            assert_eq!(
                action["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                fields(&properties)
            );
            assert_eq!(required(action), fields(&mandatory));
            for field in properties.into_iter().filter(|field| *field != "type") {
                let expected = if matches!(field, "column" | "row" | "repeat") {
                    "integer"
                } else {
                    "string"
                };
                assert_eq!(
                    action["properties"][field]["type"], expected,
                    "{kind}/{field}: preserve the field type"
                );
            }
        }
    }
}
