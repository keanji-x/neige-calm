use super::*;
use serde_json::{Value, json};

fn released_record(version: u8) -> Value {
    json!({
        "version": version,
        "execution_id": ExecutionId::fresh().unwrap(),
        "release": "Held",
        "decision": "Observing",
        "verdict": "Unprobed",
        "probes": {"verdict": null, "output": null}
    })
}

#[test]
fn forge_workspace_record_rejects_missing_v2_environment_during_decode() {
    let value = released_record(2);
    assert!(serde_json::from_value::<Record>(value).is_err());
}

#[test]
fn forge_workspace_record_rejects_malformed_v2_environment_during_decode() {
    for environment in [
        Value::Null,
        json!({}),
        json!({"source": "/project"}),
        json!({"source": "/project", "cwd": "/checkout"}),
        json!({"source": "relative-project", "cwd": "/checkout", "keys": []}),
        json!({"source": "/project", "cwd": "relative-checkout", "keys": []}),
        json!({"source": "/project", "cwd": "/checkout", "keys": null}),
        json!({"source": "/project", "cwd": "/checkout", "keys": ["Z", "A"]}),
        json!({"source": "/project", "cwd": "/checkout", "keys": ["A", "A"]}),
        json!({"source": "/project", "cwd": "/checkout", "keys": ["BAD;exit"]}),
        json!({"source": "/project", "cwd": "/checkout", "keys": ["IFS"]}),
    ] {
        let mut value = released_record(2);
        value["environment"] = environment;
        assert!(serde_json::from_value::<Record>(value).is_err());
    }
}

#[test]
fn forge_workspace_record_preserves_v1_cas_wire_shape() {
    let value = released_record(1);
    let expected = serde_json::to_string(&value).unwrap();
    let mut decoded: Record = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(encoded(&decoded).unwrap(), expected);
    decoded.release = Release::Releasing {
        authorized_at_ms: 41,
    };
    let mut transitioned = value;
    transitioned["release"] = json!({"Releasing": {"authorized_at_ms": 41}});
    assert_eq!(
        encoded(&decoded).unwrap(),
        serde_json::to_string(&transitioned).unwrap()
    );
}

#[test]
fn forge_workspace_record_roundtrips_required_v2_environment() {
    let mut value = released_record(2);
    value["environment"] = json!({
        "source": "/project", "cwd": "/checkout", "keys": ["GH_TOKEN", "PATH"]
    });
    let decoded: Record = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
}

#[test]
fn forge_workspace_record_rejects_unknown_or_mixed_protocols() {
    let unknown = released_record(3);
    assert!(serde_json::from_value::<Record>(unknown).is_err());
    let mut mixed = released_record(1);
    mixed["environment"] = json!({"source": "/project", "cwd": "/checkout", "keys": []});
    assert!(serde_json::from_value::<Record>(mixed).is_err());
}
