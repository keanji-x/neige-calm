//! Literal CLI probes, independent of the request builder and wire structs.
use super::discovery::{catalog_initialize_line, decode_auth_status, decode_catalog};

const PROBE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"neige-model-catalog","response":{"account":{"email":"private@example.invalid","organization":"private-org"},"models":[{"value":"default","resolvedModel":"claude-opus","displayName":"Default","description":"Default description","supportedEffortLevels":["low","high"]},{"value":"haiku","resolvedModel":"claude-haiku","displayName":"Haiku","description":"Fast","supportedEffortLevels":null}]}}}"#;

#[test]
fn literal_initialize_request_and_answer_preserve_every_projected_field() {
    assert_eq!(
        catalog_initialize_line(),
        "{\"request\":{\"subtype\":\"initialize\"},\"request_id\":\"neige-model-catalog\",\"type\":\"control_request\"}\n"
    );
    let catalog = decode_catalog(PROBE.as_bytes()).unwrap();
    let default = &catalog.default;
    assert_eq!(default.value, "default");
    assert_eq!(default.resolved_model, "claude-opus");
    assert_eq!(default.display_name, "Default");
    assert_eq!(default.description, "Default description");
    assert_eq!(default.effort_levels, ["low", "high"]);
    assert_eq!(catalog.models.len(), 1);
    let haiku = &catalog.models[0];
    assert_eq!(haiku.value, "haiku");
    assert_eq!(haiku.resolved_model, "claude-haiku");
    assert_eq!(haiku.display_name, "Haiku");
    assert_eq!(haiku.description, "Fast");
    assert!(haiku.effort_levels.is_empty());
    let projected = format!("{catalog:?}");
    assert!(!projected.contains("private@example.invalid"));
    assert!(!projected.contains("private-org"));
    // Account fields have no schema and cannot prevent decoding models.
    let changed = PROBE.replace(
        r#"{"email":"private@example.invalid","organization":"private-org"}"#,
        r#"[false,42,{"loggedIn":"private@example.invalid"}]"#,
    );
    assert_eq!(decode_catalog(changed.as_bytes()).unwrap(), catalog);
}

#[test]
fn catalog_list_validation_has_exact_safe_reasons() {
    let probe: serde_json::Value = serde_json::from_str(PROBE).unwrap();
    let entries = probe["response"]["response"]["models"].as_array().unwrap();
    for (models, expected) in [
        (vec![], "it lists no `default` entry"),
        (
            vec![entries[0].clone()],
            "it lists no model besides the default",
        ),
        (vec![entries[1].clone()], "it lists no `default` entry"),
        (
            vec![entries[0].clone(), entries[0].clone(), entries[1].clone()],
            "it lists `default` twice",
        ),
        (
            vec![entries[0].clone(), entries[1].clone(), entries[1].clone()],
            "it lists `haiku` twice",
        ),
    ] {
        let mut value = probe.clone();
        value["response"]["response"]["models"] = models.into();
        assert_eq!(
            decode_catalog(value.to_string().as_bytes()).unwrap_err(),
            expected
        );
    }
}

#[test]
fn malformed_catalog_envelopes_and_entries_never_echo_account_fields() {
    for (input, expected) in [
        ("".into(), "it never answered `initialize`"),
        (
            "private@example.invalid".into(),
            "a line of its output is not a record",
        ),
        (
            r#"{"type":"control_response","response":{"account":"private@example.invalid"}}"#
                .into(),
            "a control response carries no request id and subtype",
        ),
        (
            PROBE.replace("neige-model-catalog", "other-id"),
            "it never answered `initialize`",
        ),
        (
            PROBE.replace("success", "error"),
            "it answered `initialize` with an error",
        ),
        (
            PROBE.replace("\"models\"", "\"private@example.invalid\""),
            "its `initialize` answer carries no well-formed `models` list",
        ),
        (
            format!("{PROBE}\n{PROBE}"),
            "it answered `initialize` twice",
        ),
        (
            PROBE.replace("\"value\":\"haiku\"", "\"value\":\" \""),
            "entry 1 has an empty `value`",
        ),
        (
            PROBE.replace(
                "\"resolvedModel\":\"claude-haiku\"",
                "\"resolvedModel\":\"\"",
            ),
            "entry 1 has an empty `resolvedModel`",
        ),
        (
            PROBE.replace("\"displayName\":\"Haiku\"", "\"displayName\":\"\""),
            "entry 1 has an empty `displayName`",
        ),
        (
            PROBE.replace("[\"low\",\"high\"]", "[\"low\",\"low\"]"),
            "entry 0 declares effort level `low` twice",
        ),
        (
            PROBE.replace(r#"["low","high"]"#, r#"[" "]"#),
            "entry 0 declares an empty effort level",
        ),
        (
            PROBE.replace("[\"low\",\"high\"]", "[1]"),
            "its `initialize` answer carries no well-formed `models` list",
        ),
    ] {
        let reason = decode_catalog(input.as_bytes()).unwrap_err();
        assert_eq!(reason, expected);
        assert!(!reason.contains("private@example.invalid"));
        assert!(!reason.contains("private-org"));
    }
    assert_eq!(
        decode_catalog(&[0xff]).unwrap_err(),
        "its output is not UTF-8"
    );
}

#[test]
fn auth_reads_only_literal_booleans_and_never_echoes_identity() {
    for (input, expected) in [
        (
            r#"{"loggedIn":true,"email":"private@example.invalid","organization":{"secret":"private-org"}}"#,
            true,
        ),
        (r#"{"loggedIn":false,"account":[42,false]}"#, false),
    ] {
        assert_eq!(decode_auth_status(input.as_bytes()).unwrap(), expected);
    }
    for input in [
        r#"{"email":"private@example.invalid"}"#,
        r#"{"loggedIn":"private@example.invalid"}"#,
        r#"{"loggedIn":null,"organization":"private-org"}"#,
        r#"{"loggedIn":1}"#,
        r#"{"loggedIn":true,"loggedIn":false}"#,
        "private@example.invalid",
        "",
    ] {
        let reason = decode_auth_status(input.as_bytes()).unwrap_err();
        assert_eq!(reason, "printed no boolean `loggedIn`");
    }
    assert_eq!(
        decode_auth_status(&[0xff]).unwrap_err(),
        "printed no boolean `loggedIn`"
    );
}
