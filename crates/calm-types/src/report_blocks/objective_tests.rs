use super::*;
use serde_json::{Value, json};

fn payload() -> Value {
    json!({"key":"build","title":"Build","mode":"converge",
        "checks":[{"id":"tests","role":"goal","config":{"kind":"command","params":{"command":"make test"}}}],
        "constraints":[],"budget":{"turns":10,"tokens":1000,"wall_secs":60},"compact":"none"})
}

#[test]
fn objective_payload_refuses_unknown_defaulted_and_positional_fields() {
    let valid = payload();
    assert!(validate_payload("objective", &valid).is_ok());
    let mut unknown = valid.clone();
    unknown["unknown"] = json!(true);
    let mut nested = valid.clone();
    nested["checks"][0]["config"]["params"]["unknown"] = json!(true);
    let mut positional = valid.clone();
    positional["budget"] = json!([10, 1000, 60]);
    let mut defaulted = valid.clone();
    defaulted["checks"][0]["config"] = json!({"kind":"evaluator","params":{"rubric":[{"id":"r","criterion":"ok"}],"aggregate":"majority"}});
    for invalid in [unknown, nested, positional, defaulted] {
        assert!(
            validate_payload("objective", &invalid).is_err(),
            "{invalid}"
        );
    }
    let mut metric = valid;
    metric["checks"][0]["config"] =
        json!({"kind":"metric","params":{"command":"bench","direction":"lower","tolerance":1.0}});
    assert!(validate_payload("objective", &metric).is_ok());
    metric["checks"][0]["config"]["params"]["tolerance"] = json!(1);
    assert!(validate_payload("objective", &metric).is_err());
}

#[test]
fn objective_payload_delegates_semantic_diagnostics() {
    let mut value = payload();
    value["budget"]["turns"] = json!(0);
    assert!(
        validate_payload("objective", &value)
            .unwrap_err()
            .contains("budget.turns")
    );
    assert!(
        validate_payload("objective", &value)
            .unwrap_err()
            .contains("zero_budget")
    );
    for key in ["UPPER", "has space", "", "-bad"] {
        value = payload();
        value["key"] = json!(key);
        assert!(validate_payload("objective", &value).is_err());
    }
    value = payload();
    value["title"] = json!("a".repeat(MAX_STRING_CHARS + 1));
    assert!(validate_payload("objective", &value).is_err());
}

#[test]
fn objective_in_sections_excludes_objective() {
    assert!(!in_sections("objective"));
}

#[test]
fn duplicate_objective_keys_in_one_document_are_refused() {
    let block = crate::track_report::ReportBlock {
        id: "b_1111".into(),
        kind: "objective".into(),
        rev: 0,
        payload: payload(),
    };
    let mut second = block.clone();
    second.id = "b_2222".into();
    assert!(
        objectives::objective_blocks(&[block, second])
            .unwrap_err()
            .contains("duplicate objective")
    );
}
