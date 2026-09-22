use super::kinds::{KIND_VIEW, validate_payload};
use serde_json::Value;

#[test]
fn native_view_numeric_spelling_survives_kernel_canonicalization() {
    for case in fixture()["canonical_sizes"].as_array().unwrap() {
        let value: Value = serde_json::from_str(case["json"].as_str().unwrap()).unwrap();
        let canonical = super::canonical_json(&value);
        assert_eq!(canonical, case["canonical"].as_str().unwrap());
        assert!(canonical.len() >= case["decoded_bytes"].as_u64().unwrap() as usize);
    }
}

#[test]
fn native_view_table_size_uses_the_canonical_report_rendering() {
    let columns: Vec<_> = (0..32)
        .map(|i| serde_json::json!({"key": format!("c{i}"), "label": format!("Column {i}")}))
        .collect();
    let row: serde_json::Map<String, Value> = (0..32)
        .map(|i| (format!("c{i}"), serde_json::json!(12345)))
        .collect();
    let view = serde_json::json!({"version": 1, "title": "", "description": "",
        "snapshot": {"id": "size", "observedAt": 0, "producedAt": 0},
        "rows": [{"id": "row", "title": "", "layout": "one", "cells": [{"kind": "table", "id": "table", "title": "",
            "table": {"columns": columns, "rows": vec![Value::Object(row); 500]}}]}]});
    assert!(serde_json::to_vec(&view).unwrap().len() < super::MAX_CANONICAL_BYTES);
    assert!(super::canonical_json(&view).len() > super::MAX_CANONICAL_BYTES);
    assert!(
        validate_payload(KIND_VIEW, &view)
            .unwrap_err()
            .contains("payload too large")
    );
}

#[test]
fn native_view_shared_canonical_byte_boundary() {
    let fixture = fixture();
    let boundary = &fixture["budget_boundary"];
    let count = boundary["rows"].as_u64().unwrap() as usize;
    let base = boundary["empty_canonical_bytes"].as_u64().unwrap() as usize;
    for target in boundary["sizes"].as_array().unwrap() {
        let target = target.as_u64().unwrap() as usize;
        let mut view = serde_json::json!({"version": 1, "title": "", "description": "",
            "snapshot": {"id": "size", "observedAt": 0, "producedAt": 0},
            "rows": [{"id": "row", "title": "", "layout": "one", "cells": [{"kind": "table", "id": "table", "title": "",
                "table": {"columns": [{"key": "value", "label": "Value"}], "rows": vec![serde_json::json!({"value": ""}); count]}}]}]});
        assert_eq!(super::canonical_json(&view).len(), base);
        let padding = target - base;
        for (index, row) in view["rows"][0]["cells"][0]["table"]["rows"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            row["value"] = Value::String(
                "x".repeat(padding / count + if index == 0 { padding % count } else { 0 }),
            );
        }
        assert_eq!(super::canonical_json(&view).len(), target);
        assert_eq!(
            validate_payload(KIND_VIEW, &view).is_ok(),
            target <= super::MAX_CANONICAL_BYTES
        );
    }
}

#[test]
fn native_view_numeric_spellings_at_the_kernel_byte_limit() {
    let fixture = fixture();
    let count = fixture["budget_boundary"]["rows"].as_u64().unwrap() as usize;
    let base = fixture["budget_boundary"]["empty_canonical_bytes"]
        .as_u64()
        .unwrap() as usize;
    for scalar in fixture["canonical_sizes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["view_boundary"] == true)
    {
        let number: Value = serde_json::from_str(scalar["json"].as_str().unwrap()).unwrap();
        let scalar_bytes = scalar["canonical"].as_str().unwrap().len();
        let mut rows = vec![serde_json::json!({"value": ""}); count];
        rows[0]["value"] = number.clone();
        let mut view = serde_json::json!({"version": 1, "title": "", "description": "",
            "snapshot": {"id": "size", "observedAt": 0, "producedAt": 0},
            "rows": [{"id": "row", "title": "", "layout": "one", "cells": [{"kind": "table", "id": "table", "title": "",
                "table": {"columns": [{"key": "value", "label": "Value"}], "rows": rows}}]}]});
        assert_eq!(super::canonical_json(&view).len(), base - 2 + scalar_bytes);
        let padding = super::MAX_CANONICAL_BYTES - (base - 2 + scalar_bytes);
        for (index, row) in view["rows"][0]["cells"][0]["table"]["rows"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
            .skip(1)
        {
            row["value"] = Value::String("x".repeat(
                padding / (count - 1) + if index == 1 { padding % (count - 1) } else { 0 },
            ));
        }
        assert_eq!(
            super::canonical_json(&view).len(),
            super::MAX_CANONICAL_BYTES
        );
        validate_payload(KIND_VIEW, &view).unwrap();
        assert_eq!(
            view["rows"][0]["cells"][0]["table"]["rows"][0]["value"],
            number
        );
    }
}

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
