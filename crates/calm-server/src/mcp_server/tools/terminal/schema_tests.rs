use crate::mcp_server::build_default_registry;
use crate::model::CardRole;
use crate::terminal_permissions::{
    CLAUDE_PERMISSIONS_BASH_MAX, CLAUDE_PERMISSIONS_DENY_MAX, CLAUDE_PERMISSIONS_EDIT_MAX,
    CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS,
};
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
    let descriptors = build_default_registry().descriptors_for_role(CardRole::Planner);
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
    const WAIT: [&str; 7] = [
        "wait_ms",
        "wait_for",
        "settle_ms",
        "signal_events",
        "repaint_ms",
        "wait_text",
        "wait_text_absent",
    ];
    const SCROLL_TO: [&str; 2] = ["scroll_to_text", "scroll_to_occurrence"];
    for (name, common, mandatory) in [
        ("resolve", vec![], vec![]),
        (
            "observe",
            [&["scroll_offset", "format"][..], &SCROLL_TO[..], &WAIT[..]].concat(),
            vec![],
        ),
        (
            "control",
            [&["action", "observe"][..], &WAIT[..]].concat(),
            vec!["action"],
        ),
        (
            "input",
            [
                &[
                    "observation_id",
                    "request_id",
                    "action",
                    "observe",
                    "allow_output_since_observation",
                    "allow_output_below_cursor",
                    "claim",
                    "release",
                ][..],
                &WAIT[..],
            ]
            .concat(),
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
        assert_eq!(schema["additionalProperties"], false, "{name}");
        assert_eq!(
            required(schema),
            fields(&mandatory),
            "{name}: root required fields; neither selector is required"
        );
        let mut expected = fields(&common);
        expected.insert("terminal_id".into());
        expected.insert("task_id".into());
        assert_eq!(
            properties(schema),
            expected,
            "{name}: all common properties and both optional selectors"
        );
        for selector in ["terminal_id", "task_id"] {
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
                .starts_with("Select exactly one terminal_id or task_id")
                || descriptor
                    .description
                    .starts_with("Resolve exactly one task_id"),
            "{name}: exactly-one targeting is the first sentence"
        );
        if name != "calm.terminal.resolve" {
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
        .find(|descriptor| descriptor.name == "calm.terminal.open")
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
                &[
                    "request_id",
                    "title",
                    "program",
                    "format",
                    "claim",
                    "claude_permissions"
                ][..],
                &WAIT[..]
            ]
            .concat()
        )
    );
    assert_eq!(required(open_schema), fields(&["request_id"]));
    let permissions = &open_schema["properties"]["claude_permissions"];
    assert_eq!(permissions["type"], "object");
    assert_eq!(permissions["additionalProperties"], false);
    assert_eq!(properties(permissions), fields(&["edit", "bash", "deny"]));
    assert!(permissions.get("required").is_none());
    let entry = json!({"type":"string","minLength":1,"maxLength":200});
    for (list, min_items, max_items) in [
        ("edit", Some(1), CLAUDE_PERMISSIONS_EDIT_MAX),
        ("bash", Some(1), CLAUDE_PERMISSIONS_BASH_MAX),
        ("deny", None, CLAUDE_PERMISSIONS_DENY_MAX),
    ] {
        let schema = &permissions["properties"][list];
        assert_eq!(schema["type"], "array", "{list}");
        assert_eq!(schema["items"], entry, "{list}");
        assert_eq!(schema["maxItems"], json!(max_items), "{list}");
        assert_eq!(
            schema.get("minItems").cloned(),
            min_items.map(|n| json!(n)),
            "{list}"
        );
    }
    assert_eq!(
        entry["maxLength"],
        json!(CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS)
    );
    let observe_schema = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "calm.terminal.observe")
        .unwrap()
        .input_schema;
    for property in WAIT {
        assert_eq!(
            open_schema["properties"][property], observe_schema["properties"][property],
            "open/{property}: the same wait contract as observe"
        );
    }
    let bytes = open_schema.to_string().len();
    assert!(
        bytes < 4000,
        "open: {bytes} bytes; avoid model schema compaction"
    );
    assert_eq!(
        observe_schema["properties"]["scroll_to_text"],
        json!({"type":"string","minLength":1,"maxLength":200})
    );
    assert_eq!(
        observe_schema["properties"]["scroll_to_occurrence"],
        json!({"type":"string","enum":["latest","earliest"],"default":"latest"})
    );
    for name in ["open", "control", "input", "resolve"] {
        let schema = &descriptors
            .iter()
            .find(|descriptor| descriptor.name == format!("calm.terminal.{name}"))
            .unwrap()
            .input_schema;
        for property in SCROLL_TO {
            assert!(
                schema["properties"].get(property).is_none(),
                "{name}/{property}: the history search is observe's alone"
            );
        }
    }
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "calm.terminal.input")
        .unwrap()
        .input_schema;
    for flag in [
        "allow_output_since_observation",
        "allow_output_below_cursor",
        "claim",
        "release",
    ] {
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
    let actions = input["properties"]["action"]["anyOf"].as_array().unwrap();
    assert_eq!(actions.len(), 5);
    for (action, kind, properties, mandatory) in [
        (
            &actions[0],
            json!(["text", "submit"]),
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
            json!("click"),
            vec!["type", "column", "row"],
            vec!["type", "column", "row"],
        ),
        (
            &actions[3],
            json!("sequence"),
            vec!["type", "steps"],
            vec!["type", "steps"],
        ),
        (
            &actions[4],
            json!("replace"),
            vec!["type", "from", "to"],
            vec!["type", "from", "to"],
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
                "column" | "row" | "repeat" => "integer",
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
        actions[3]["properties"]["steps"],
        json!({"type":"array","minItems":2,"maxItems":8,"items":{"type":"object"}})
    );
    assert_eq!(
        actions[4]["properties"]["from"],
        json!({"type":"string","minLength":1,"maxLength":200})
    );
    assert_eq!(
        actions[4]["properties"]["to"],
        json!({"type":"string","maxLength":16384})
    );
}
