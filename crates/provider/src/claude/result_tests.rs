//! Success result optionality follows the SDK's ResultMessage contract.

use serde_json::{Value, json};

use super::protocol::{ProtocolError, Record, decode};

fn success() -> Value {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "done",
        "usage": {
            "input_tokens": 0, "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0, "output_tokens": 0, "iterations": []
        },
        "modelUsage": {}
    })
}

#[test]
fn success_without_terminal_reason_decodes() {
    let Record::ResultSuccess(result) = decode(&success().to_string()).unwrap() else {
        panic!("expected success result");
    };
    assert_eq!(result.terminal_reason, None);
}

#[test]
fn success_with_null_terminal_reason_decodes() {
    let mut value = success();
    value["terminal_reason"] = Value::Null;
    let Record::ResultSuccess(result) = decode(&value.to_string()).unwrap() else {
        panic!("expected success result");
    };
    assert_eq!(result.terminal_reason, None);
}

#[test]
fn success_preserves_explicit_terminal_reason() {
    for reason in ["completed", "aborted_streaming", "future_reason", ""] {
        let mut value = success();
        value["terminal_reason"] = json!(reason);
        let Record::ResultSuccess(result) = decode(&value.to_string()).unwrap() else {
            panic!("expected success result");
        };
        assert_eq!(result.terminal_reason.as_deref(), Some(reason));
    }
}

#[test]
fn success_rejects_non_string_terminal_reason() {
    for reason in [json!(true), json!(42), json!([]), json!({})] {
        let mut value = success();
        value["terminal_reason"] = reason;
        assert!(
            matches!(decode(&value.to_string()), Err(ProtocolError::Shape { .. })),
            "{value}"
        );
    }
}

#[test]
fn success_keeps_other_fields_required_and_typed() {
    let value = success();
    decode(&value.to_string()).expect("valid baseline without terminal_reason");
    for field in ["is_error", "result", "usage", "modelUsage"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            matches!(
                decode(&missing.to_string()),
                Err(ProtocolError::Shape { .. })
            ),
            "{missing}"
        );
        let mut wrong = value.clone();
        wrong[field] = json!([]);
        assert!(
            matches!(decode(&wrong.to_string()), Err(ProtocolError::Shape { .. })),
            "{wrong}"
        );
    }
    for field in [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
        "iterations",
    ] {
        let mut missing = value.clone();
        missing["usage"].as_object_mut().unwrap().remove(field);
        assert!(
            matches!(
                decode(&missing.to_string()),
                Err(ProtocolError::Shape { .. })
            ),
            "{missing}"
        );
        let mut wrong = value.clone();
        wrong["usage"][field] = json!("invalid");
        assert!(
            matches!(decode(&wrong.to_string()), Err(ProtocolError::Shape { .. })),
            "{wrong}"
        );
    }
}
