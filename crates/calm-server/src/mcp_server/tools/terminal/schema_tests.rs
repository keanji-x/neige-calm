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
fn properties(schema: &Value) -> BTreeSet<String> {
    schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// Every tool's input schema must stay under the local 4000-byte compaction threshold.
#[test]
fn terminal_discovery_is_flat_with_optional_selectors_and_closed_action_arms() {
    let descriptors = build_default_registry().descriptors_listed_for(CardRole::Planner);
    let terminal_schemas: serde_json::Map<String, Value> = descriptors
        .iter()
        .filter(|descriptor| descriptor.name.starts_with("neige_terminal_"))
        .map(|descriptor| (descriptor.name.clone(), descriptor.input_schema.clone()))
        .collect();
    println!("TERMINAL_TOOL_SCHEMAS={}", Value::Object(terminal_schemas));
    assert_eq!(
        descriptors
            .iter()
            .filter(|descriptor| descriptor.name.starts_with("neige_terminal_")
                && descriptor.name != "neige_terminal_open")
            .map(|descriptor| descriptor.name.clone())
            .collect::<BTreeSet<_>>(),
        fields(&[
            "neige_terminal_show",
            "neige_terminal_read",
            "neige_terminal_control",
            "neige_terminal_input"
        ]),
        "every registered targeted Terminal tool must be covered by this sweep"
    );
    const WAIT: [&str; 7] = [
        "wait_ms",
        "wait_for",
        "settle_ms",
        "signal_events",
        "repaint_ms",
        "wait_text",
        "wait_text_absent",
    ];
    for (name, common, mandatory) in [
        ("show", vec![], vec![]),
        ("read", [&["scroll_offset"][..], &WAIT[..]].concat(), vec![]),
        (
            "control",
            [&["action", "read"][..], &WAIT[..]].concat(),
            vec!["action"],
        ),
        (
            "input",
            [
                &[
                    "observation_id",
                    "idempotency_key",
                    "action",
                    "read",
                    "allow_output_since_observation",
                    "claim",
                    "release",
                ][..],
                &WAIT[..],
            ]
            .concat(),
            vec!["idempotency_key", "action"],
        ),
    ] {
        let name = format!("neige_terminal_{name}");
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.name == name)
            .unwrap();
        let schema = &descriptor.input_schema;
        assert_eq!(
            schema["type"], "object",
            "{name}: MCP root remains an object"
        );
        assert_eq!(schema["additionalProperties"], false, "{name}");
        assert_eq!(
            required(schema),
            fields(&mandatory),
            "{name}: root required fields; neither selector is required"
        );
        let mut expected = fields(&common);
        expected.insert("terminal_id".into());
        expected.insert("attempt_id".into());
        assert_eq!(
            properties(schema),
            expected,
            "{name}: all common properties and both optional selectors"
        );
        for selector in ["terminal_id", "attempt_id"] {
            assert_eq!(
                schema["properties"][selector],
                json!({"type":"string"}),
                "{name}/{selector}"
            );
        }
        assert!(
            schema.get("anyOf").is_none() && schema.get("oneOf").is_none(),
            "{name}: no root union arms (the local transformer does not model oneOf, and anyOf arms tripled the schema)"
        );
        assert!(
            !schema.to_string().contains("\"oneOf\""),
            "{name}: local transformer does not model oneOf"
        );
        let bytes = schema.to_string().len();
        assert!(
            bytes < 4000,
            "{name}: {bytes} bytes; avoid model schema compaction"
        );
        assert!(
            descriptor
                .description
                .starts_with("Select exactly one terminal_id or attempt_id")
                || descriptor
                    .description
                    .starts_with("Resolve exactly one attempt_id"),
            "{name}: exactly-one targeting is the first sentence"
        );
        if name != "neige_terminal_show" {
            // The omitted-budget default depends on wait_for, so it is stated in the description rather than as a JSON Schema default.
            assert_eq!(
                schema["properties"]["wait_ms"],
                json!({"type":"integer","minimum":0,"maximum":20000}),
                "{name}: the per-mode defaults live in the description (schema bytes)"
            );
            assert_eq!(
                schema["properties"]["wait_for"],
                json!({"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["signal_events"],
                json!({"type":"array","minItems":1,"items":{"type":"string"}}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["wait_text"],
                json!({"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["wait_text_absent"], schema["properties"]["wait_text"],
                "{name}"
            );
            assert_eq!(
                schema["properties"]["settle_ms"],
                json!({"type":"integer","minimum":0,"maximum":2000,"default":150}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["repaint_ms"],
                json!({"type":"integer","minimum":0,"maximum":5000}),
                "{name}"
            );
        }
    }
    let open_schema = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "neige_terminal_open")
        .unwrap()
        .input_schema;
    assert_eq!(
        open_schema["properties"]["claim"],
        json!({"type":"boolean","default":false})
    );
    assert_eq!(
        properties(open_schema),
        fields(
            &[
                &["idempotency_key", "title", "program", "claim"][..],
                &WAIT[..]
            ]
            .concat()
        )
    );
    assert_eq!(required(open_schema), fields(&["idempotency_key"]));
    let read_schema = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "neige_terminal_read")
        .unwrap()
        .input_schema;
    for property in WAIT {
        assert_eq!(
            open_schema["properties"][property], read_schema["properties"][property],
            "open/{property}: the same wait contract as read"
        );
    }
    let bytes = open_schema.to_string().len();
    assert!(
        bytes < 4000,
        "open: {bytes} bytes; avoid model schema compaction"
    );
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "neige_terminal_input")
        .unwrap()
        .input_schema;
    for flag in ["allow_output_since_observation", "claim", "release"] {
        assert_eq!(
            input["properties"][flag],
            json!({"type":"boolean","default":false}),
            "{flag}"
        );
    }
    assert_eq!(
        input["properties"]["observation_id"],
        json!({"type":"string","format":"uuid"}),
        "observation_id stays typed while optional"
    );
}

/// The input action union carries exactly the live actions, each a closed arm.
#[test]
fn input_schema_lists_only_live_actions() {
    let descriptors = build_default_registry().descriptors_listed_for(CardRole::Planner);
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "neige_terminal_input")
        .unwrap()
        .input_schema;
    let actions = input["properties"]["action"]["anyOf"].as_array().unwrap();
    let live: Vec<Value> = actions
        .iter()
        .map(|action| action["properties"]["type"].clone())
        .collect();
    assert_eq!(
        live,
        [
            json!({"enum":["text","submit","message"]}),
            json!({"const":"key"}),
            json!({"const":"sequence"}),
        ],
        "exactly the live actions; replace and click are deleted"
    );
    for (action, kind, properties, mandatory) in [
        (
            &actions[0],
            json!(["text", "submit", "message"]),
            vec!["type", "text"],
            vec!["type", "text"],
        ),
        (
            &actions[1],
            json!("key"),
            vec!["type", "key", "repeat"],
            vec!["type", "key"],
        ),
        (
            &actions[2],
            json!("sequence"),
            vec!["type", "steps"],
            vec!["type", "steps"],
        ),
    ] {
        assert_eq!(action["type"], "object");
        assert_eq!(action["additionalProperties"], false);
        match &kind {
            Value::Array(values) => {
                assert_eq!(action["properties"]["type"], json!({"enum":values}))
            }
            other => assert_eq!(action["properties"]["type"], json!({"const":other})),
        }
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
            let expected = match field {
                "repeat" => "integer",
                "steps" => "array",
                _ => "string",
            };
            assert_eq!(
                action["properties"][field]["type"], expected,
                "{kind}/{field}: preserve the field type"
            );
        }
    }
    assert_eq!(
        actions[2]["properties"]["steps"],
        json!({"type":"array","minItems":2,"maxItems":8,"items":{"type":"object"}})
    );
}

/// `message`'s valid and refused option lists are exactly `neige_terminal_input`'s properties.
#[test]
fn message_option_lists_cover_the_input_schema() {
    let descriptors = build_default_registry().descriptors_listed_for(CardRole::Planner);
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "neige_terminal_input")
        .unwrap()
        .input_schema;
    let schema: BTreeSet<String> = input["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let valid: BTreeSet<String> = super::MESSAGE_OPTIONS
        .split(", ")
        .map(str::to_owned)
        .collect();
    let refused: BTreeSet<String> = super::MESSAGE_REFUSED_OPTIONS
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert!(valid.is_disjoint(&refused));
    assert_eq!(&valid | &refused, schema);
}
