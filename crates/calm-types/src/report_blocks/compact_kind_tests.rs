use super::*;
use serde_json::json;

#[test]
fn compact_task_kind_has_only_planner_declaration_fields() {
    let minimal = json!({"key":"compact","kind":"compact","ready":true,"declared_by":"spec"});
    assert_eq!(validate_payload(KIND_TASK, &minimal), Ok(()));
    let mut released = minimal.clone();
    released["released_by_user"] = json!(true);
    assert_eq!(validate_payload(KIND_TASK, &released), Ok(()));
    for field in TASK_FIELDS.iter().filter(|field| {
        !["key", "kind", "ready", "declared_by", "released_by_user"].contains(field)
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
