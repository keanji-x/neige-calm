use super::*;
use serde_json::json;

fn manifest(tool: Value) -> Value {
    json!({"manifest_version":1,"id":"test.assistant","version":"0.1.0",
      "min_kernel_version":"0.0.1","display_name":"Assistant fixture",
      "entrypoint":{"command":"bin/test"},"exposes_tools":[tool]})
}

#[test]
fn assistant_access_defaults_false_and_roundtrips_explicit_opt_in() {
    for (tool, expected) in [
        (json!({"name":"read"}), false),
        (json!({"name":"read","assistant_access":true}), true),
    ] {
        let parsed = Manifest::parse(&manifest(tool).to_string()).unwrap();
        let roundtrip = Manifest::parse(&parsed.to_json().to_string()).unwrap();
        assert_eq!(
            roundtrip.to_json()["exposes_tools"][0]["assistant_access"],
            expected
        );
    }
}

#[test]
fn duplicate_exposed_tool_names_are_rejected_regardless_of_grant_order() {
    for grants in [[false, true], [true, false], [false, false], [true, true]] {
        let mut value = manifest(json!({"name":"read","assistant_access":grants[0]}));
        value["exposes_tools"].as_array_mut().unwrap().push(json!({
            "name":"read", "assistant_access":grants[1],
        }));
        let error =
            Manifest::parse(&value.to_string()).expect_err("ambiguous names must not install");
        assert!(
            error.to_string().contains("exposes_tools[1].name"),
            "{error}"
        );
        assert!(error.to_string().contains("duplicate"), "{error}");
        value["exposes_tools"][1]["name"] = json!("another");
        Manifest::parse(&value.to_string()).expect("distinct ordinary tools remain valid");
    }
}

#[test]
fn assistant_access_rejects_execution_backed_tools() {
    let value = manifest(json!({"name":"execute","kind":"forge-action","assistant_access":true}));
    let error = Manifest::parse(&value.to_string()).expect_err("forge access must be rejected");
    assert!(error.to_string().contains("assistant_access"), "{error}");
    let mut control = value;
    control["exposes_tools"][0]["assistant_access"] = json!(false);
    Manifest::parse(&control.to_string()).expect("legacy forge tool remains valid");
}

#[test]
fn assistant_access_rejects_http_and_cli_connectors() {
    for (kind, key, block) in [
        (
            "mcp-http",
            "mcp_http",
            json!({"url":"https://example.test/mcp","tools_allow":["read"]}),
        ),
        (
            "cli-query",
            "cli_query",
            json!({"command":"/usr/bin/test","tools":[{"name":"read","input_schema":{},"args":[]}]}),
        ),
    ] {
        let mut value = manifest(json!({"name":"read","assistant_access":true}));
        value.as_object_mut().unwrap().remove("entrypoint");
        value["kind"] = json!(kind);
        value[key] = block;
        let error = Manifest::parse(&value.to_string()).expect_err("connector cannot opt in");
        assert!(
            error.to_string().contains("assistant_access"),
            "{kind}: {error}"
        );
        value["exposes_tools"][0]["assistant_access"] = json!(false);
        Manifest::parse(&value.to_string()).expect("ordinary connector remains valid");
    }
}
