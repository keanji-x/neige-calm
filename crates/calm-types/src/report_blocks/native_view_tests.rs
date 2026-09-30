use super::kinds::{KIND_VIEW, validate_payload};
use serde_json::Value;

#[test]
fn native_view_shared_neutral_palette() {
    let fixture = fixture();
    let mut view = fixture["valid"].clone();
    view["rows"][0]["cells"][1]["datasets"][0]["series"][0]["palette"] =
        fixture["neutral_palette"].clone();
    validate_payload(KIND_VIEW, &view).unwrap();
}

#[test]
fn native_view_explicit_wide_layouts_require_two_cells() {
    let fixture = fixture();
    for layout in fixture["wide_layouts"].as_array().unwrap() {
        let mut view = fixture["valid"].clone();
        view["rows"][0]["layout"] = layout.clone();
        validate_payload(KIND_VIEW, &view).unwrap();
        view["rows"][0]["cells"].as_array_mut().unwrap().pop();
        assert!(validate_payload(KIND_VIEW, &view).is_err());
    }
}

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

#[test]
fn native_view_generic_records_accept_publisher_badges() {
    for badges in [
        serde_json::json!([]),
        serde_json::json!([{ "label": "State", "value": "Open", "tone": "neutral" }]),
        serde_json::json!([
            { "label": "State", "value": "Open", "tone": "neutral" },
            { "label": "Owner", "value": "Operations", "tone": "positive" },
            { "label": "Priority", "value": "Low", "tone": "warning" }
        ]),
    ] {
        let mut view = fixture()["valid"].clone();
        view["rows"] = serde_json::json!([{ "id": "items", "title": "Inventory", "layout": "one", "cells": [{
            "kind": "records", "id": "inventory", "title": "Devices", "emptyText": "None", "datasets": [{
                "id": "sample", "label": "Observed", "description": null, "items": [{
                    "id": "device", "subtitle": "Storage", "title": "Disk", "summary": "Available",
                    "badges": badges, "facts": [], "sections": [], "disclosures": []
                }]
            }]
        }] }]);
        validate_payload(KIND_VIEW, &view)
            .expect("generic records must accept publisher-defined badges");
    }
}

#[test]
fn native_view_generated_contract_matches_dtos() {
    let schema = super::native_view::schema();
    assert_eq!(schema, super::native_view::generated_schema());
    assert_eq!(
        schema["$defs"]["Snapshot"]["required"],
        serde_json::json!(["id", "observedAt", "producedAt"])
    );
    for variant in schema["$defs"]["Component"]["oneOf"].as_array().unwrap() {
        assert_eq!(variant["additionalProperties"], false);
    }
    assert!(super::native_view::typescript().contains("export type ViewRecord ="));
}

#[test]
fn native_view_snapshot_requires_explicit_nullable_times() {
    let mut view = fixture()["valid"].clone();
    for key in ["observedAt", "producedAt"] {
        view["snapshot"][key] = Value::Null;
    }
    validate_payload(KIND_VIEW, &view).unwrap();
    for key in ["observedAt", "producedAt"] {
        let mut missing = view.clone();
        missing["snapshot"].as_object_mut().unwrap().remove(key);
        assert!(validate_payload(KIND_VIEW, &missing).is_err());
    }
}

#[test]
fn native_view_generic_primitives_validate_without_business_semantics() {
    let mut view = fixture()["valid"].clone();
    view["rows"] = serde_json::json!([{ "id": "summary", "title": "", "layout": "two", "cells": [
        { "kind": "bars", "id": "bars", "title": "", "unit": "", "emptyText": "", "points": [{ "label": "A", "value": -1, "tone": "neutral" }] },
        { "kind": "meter", "id": "meter", "title": "", "unit": "", "detail": "", "used": null, "limit": null, "usedLabel": "", "limitLabel": "", "emptyText": "", "tone": "neutral" }
    ] }]);
    validate_payload(KIND_VIEW, &view).unwrap();
    for index in [0, 1] {
        let mut bad = view.clone();
        bad["rows"][0]["cells"][index]["unexpected"] = serde_json::json!(true);
        assert!(validate_payload(KIND_VIEW, &bad).is_err());
    }
    for (field, invalid) in [
        ("used", serde_json::json!(-1)),
        ("limit", serde_json::json!(0)),
    ] {
        let mut bad = view.clone();
        bad["rows"][0]["cells"][1][field] = invalid;
        assert!(validate_payload(KIND_VIEW, &bad).is_err());
        bad["rows"][0]["cells"][1]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(validate_payload(KIND_VIEW, &bad).is_err());
    }
    view["rows"][0]["cells"][0]["points"] = serde_json::json!([]);
    view["rows"][0]["cells"][1]["used"] = serde_json::json!(3);
    view["rows"][0]["cells"][1]["limit"] = serde_json::json!(2);
    validate_payload(KIND_VIEW, &view).unwrap(); // A meter may exceed its publisher-defined limit.
}

#[test]
fn native_view_read_contract_preserves_full_disclosures_and_history() {
    let mut view = fixture()["valid"].clone();
    let records = &mut view["rows"][2]["cells"][0]["datasets"][0]["items"];
    let mut record = records[0].clone();
    record["summary"] = serde_json::json!("界".repeat(8000));
    record["sections"][0]["body"] = serde_json::json!("界".repeat(8000));
    record["disclosures"][0]["body"] = serde_json::json!("界".repeat(8000));
    *records = Value::Array(
        (0..100)
            .map(|i| {
                let mut r = record.clone();
                r["id"] = serde_json::json!(format!("r{i}"));
                r
            })
            .collect(),
    );
    super::native_view::validate(&view).unwrap();
    assert!(
        validate_payload(KIND_VIEW, &view)
            .unwrap_err()
            .contains("payload too large")
    );
    view["rows"][2]["cells"][0]["datasets"][0]["items"][0]["disclosures"][0]["body"] =
        serde_json::json!("界".repeat(8001));
    assert!(super::native_view::validate(&view).is_err());
}

#[test]
fn native_view_generated_component_fields_match_wire_names() {
    let schema = super::native_view::generated_schema();
    for variant in schema["$defs"]["Component"]["oneOf"].as_array().unwrap() {
        let properties = variant["properties"].as_object().unwrap();
        assert!(!properties.contains_key("empty_text"));
        assert!(!properties.contains_key("used_label"));
        assert!(!properties.contains_key("limit_label"));
        match properties["kind"]["enum"][0].as_str().unwrap() {
            "time-series" | "distribution" | "bars" | "records" => {
                assert!(properties.contains_key("emptyText"))
            }
            "meter" => {
                for key in ["emptyText", "usedLabel", "limitLabel"] {
                    assert!(properties.contains_key(key));
                }
            }
            _ => {}
        }
    }
}

#[test]
fn native_view_fixture_components_match_generated_wire_properties() {
    let schema = super::native_view::generated_schema();
    for row in fixture()["valid"]["rows"].as_array().unwrap() {
        for cell in row["cells"].as_array().unwrap() {
            let variant = schema["$defs"]["Component"]["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|variant| variant["properties"]["kind"]["enum"][0] == cell["kind"])
                .unwrap();
            let properties = variant["properties"].as_object().unwrap();
            for key in cell.as_object().unwrap().keys() {
                assert!(
                    properties.contains_key(key),
                    "wire field {key} absent from generated contract"
                );
            }
            for key in variant["required"].as_array().unwrap() {
                assert!(cell.get(key.as_str().unwrap()).is_some());
            }
        }
    }
}

#[test]
fn native_view_generated_table_null_is_a_type_constraint() {
    let schema = super::native_view::generated_schema();
    let null = schema["$defs"]["TableScalar"]["oneOf"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    let null = if let Some(reference) = null["$ref"].as_str() {
        &schema["$defs"][reference.strip_prefix("#/$defs/").unwrap()]
    } else {
        null
    };
    assert_eq!(null["type"], "null");
}

#[test]
fn native_view_generated_optional_types_preserve_explicit_null() {
    let types = super::native_view::typescript();
    for field in [
        "description?: string | null",
        "caption?: string | null",
        "highlight?: string | null",
        "align?: TableAlign | null",
    ] {
        assert!(
            types.contains(field),
            "generated types do not preserve {field}"
        );
    }
}

#[test]
fn native_view_observation_boundaries_are_inclusive_and_nullable() {
    for value in [
        serde_json::json!(-1e15),
        serde_json::json!(1e15),
        serde_json::Value::Null,
    ] {
        let mut view = fixture()["valid"].clone();
        view["rows"][0]["cells"][1]["datasets"][0]["points"][0]["values"][0] = value;
        validate_payload(KIND_VIEW, &view).unwrap();
    }
}
