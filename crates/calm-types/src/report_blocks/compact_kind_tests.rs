use super::*;
use serde_json::json;

#[test]
fn compact_task_kind_has_only_planner_declaration_fields() {
    let minimal = json!({"key":"compact","kind":"compact", "context":"Continue from verified task evidence.","ready":true,"declared_by":"spec"});
    assert_eq!(validate_payload(KIND_TASK, &minimal), Ok(()));
    let mut released = minimal.clone();
    released["released_by_user"] = json!(true);
    assert_eq!(validate_payload(KIND_TASK, &released), Ok(()));
    for field in TASK_FIELDS.iter().filter(|field| {
        ![
            "key",
            "kind",
            "ready",
            "declared_by",
            "released_by_user",
            "context",
        ]
        .contains(field)
    }) {
        let mut invalid = minimal.clone();
        invalid[*field] = Value::Null;
        assert!(
            validate_payload(KIND_TASK, &invalid).is_err(),
            "accepted {field}"
        );
    }
    for value in [json!("user"), Value::Null, json!(false)] {
        let mut invalid = minimal.clone();
        invalid["declared_by"] = value;
        assert!(validate_payload(KIND_TASK, &invalid).is_err());
    }
}

#[test]
fn compact_task_context_is_required_non_blank_string() {
    let valid = json!({"key":"compact", "kind":"compact", "ready":true,
        "declared_by":"spec", "context":"交接\nverified head"});
    assert_eq!(validate_payload(KIND_TASK, &valid), Ok(()));
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("context");
    assert!(validate_payload(KIND_TASK, &missing).is_err());
    for value in [
        json!(""),
        json!("  \n\t"),
        json!({}),
        json!("x".repeat(2049)),
    ] {
        let mut invalid = valid.clone();
        invalid["context"] = value;
        assert!(validate_payload(KIND_TASK, &invalid).is_err(), "{invalid}");
    }
    let mut boundary = valid;
    boundary["context"] = json!("中".repeat(2048));
    assert_eq!(validate_payload(KIND_TASK, &boundary), Ok(()));
}
