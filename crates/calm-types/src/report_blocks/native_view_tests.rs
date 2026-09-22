use super::kinds::{KIND_VIEW, validate_payload};
use serde_json::Value;

pub fn fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap()
}

#[test]
fn native_view_shared_conformance() {
    let fixture = fixture();
    validate_payload(KIND_VIEW, &fixture["valid"]).unwrap();
    for change in fixture["invalid"].as_array().unwrap() {
        let mut value = fixture["valid"].clone();
        let path = change["path"].as_array().unwrap();
        let mut parent = &mut value;
        for part in &path[..path.len() - 1] {
            parent = if let Some(index) = part.as_u64() {
                &mut parent[index as usize]
            } else {
                &mut parent[part.as_str().unwrap()]
            };
        }
        let key = path.last().unwrap().as_str().unwrap();
        if change["remove"] == true {
            parent.as_object_mut().unwrap().remove(key);
        } else {
            parent[key] = change["value"].clone();
        }
        assert!(
            validate_payload(KIND_VIEW, &value).is_err(),
            "accepted {}",
            change["name"]
        );
    }
}

#[test]
fn native_view_rejects_oversized_payload_and_long_text() {
    let mut value = fixture()["valid"].clone();
    value["title"] = Value::String("x".repeat(201));
    assert!(validate_payload(KIND_VIEW, &value).is_err());
    let mut value = fixture()["valid"].clone();
    let record = &mut value["rows"][2]["cells"][0]["datasets"][0]["items"][0];
    record["summary"] = Value::String("x".repeat(2048));
    record["sections"] = serde_json::json!([{"label":"A","body":"x".repeat(2048)}]);
    let record = record.clone();
    value["rows"][2]["cells"][0]["datasets"][0]["items"] = Value::Array(
        (0..50)
            .map(|i| {
                let mut r = record.clone();
                r["id"] = Value::String(format!("r{i}"));
                r["sections"] = Value::Array(vec![
                    serde_json::json!({"label":"A","body":"x".repeat(2048)});
                    8
                ]);
                r
            })
            .collect(),
    );
    assert!(
        validate_payload(KIND_VIEW, &value)
            .unwrap_err()
            .contains("payload too large")
    );
}
