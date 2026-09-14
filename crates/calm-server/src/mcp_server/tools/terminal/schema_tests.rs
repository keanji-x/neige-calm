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

/// #1666 — the targeted tools are flat objects: both selectors are optional
/// root properties (exactly-one targeting is enforced server-side by
/// `Target::from_ids` and stated first in every description), no root
/// `anyOf`/`oneOf`, and the closed `action` arms stay intact. The whole
/// input schema of every tool stays under the local 4000-byte compaction
/// threshold, which the former selector arms (3× every property) had
/// exhausted.
#[test]
fn terminal_discovery_is_flat_with_optional_selectors_and_closed_action_arms() {
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
    const WAIT: [&str; 6] = [
        "wait_ms",
        "wait_for",
        "settle_ms",
        "signal_events",
        "repaint_ms",
        "wait_text",
    ];
    for (name, common, mandatory) in [
        ("resolve", vec![], vec![]),
        (
            "observe",
            [&["scroll_offset", "format"][..], &WAIT[..]].concat(),
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
            // #1618 waiting arguments: raised budget, change mode and settle.
            // The omitted-budget default depends on wait_for, so it is stated
            // in the description rather than as a single JSON Schema default.
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
            // #1620 signal events: signal mode only. The vocabulary is
            // validated server-side and stated in the descriptions.
            assert_eq!(
                schema["properties"]["signal_events"],
                json!({"type":"array","minItems":1,"items":{"type":"string"}}),
                "{name}"
            );
            // #1666 text patterns: text mode only (refused elsewhere
            // server-side); the bounds are the server's.
            assert_eq!(
                schema["properties"]["wait_text"],
                json!({"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}}),
                "{name}"
            );
            assert_eq!(
                schema["properties"]["settle_ms"],
                json!({"type":"integer","minimum":0,"maximum":2000,"default":150}),
                "{name}"
            );
            // #1628 repaint window: signal mode only (refused elsewhere
            // server-side; that and the 1500 default are stated in the
            // descriptions to keep the input schema under the compaction cap).
            assert_eq!(
                schema["properties"]["repaint_ms"],
                json!({"type":"integer","minimum":0,"maximum":5000}),
                "{name}"
            );
        }
    }
    // #1620: open claims in one call.
    let open_schema = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "calm.terminal.open")
        .unwrap()
        .input_schema;
    assert_eq!(
        open_schema["properties"]["claim"],
        json!({"type":"boolean","default":false})
    );
    let input = &descriptors
        .iter()
        .find(|descriptor| descriptor.name == "calm.terminal.input")
        .unwrap()
        .input_schema;
    // #1618 drift opt-in, #1666 below-cursor tolerance and control steps:
    // plain booleans, default false, validated server-side.
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
    assert_eq!(actions.len(), 4);
    // #1620: `submit` shares the text arm (same fields, same limits) as a
    // two-value discriminator instead of a separate arm. #1666: `sequence`
    // is its own arm; its steps are typed as objects here and validated
    // server-side (text or editing-key actions, 2..=8 of them).
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
}
