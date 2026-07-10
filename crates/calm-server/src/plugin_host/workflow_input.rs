//! Issue #891 — hand-rolled JSON-Schema **subset** for `WorkflowDescriptor.
//! input_schema` and the matching instance validator for `NewWave.
//! workflow_input`.
//!
//! Deliberately not the `jsonschema` crate (twice-recorded decision:
//! `manifest.rs` module doc + `calm-server/Cargo.toml` dependency notes): the
//! supported surface is a closed keyword set, small enough that hand-written
//! validation gives better error messages without a new dependency tree. The
//! subset is enforced at manifest-validation time so the instance validator
//! below never has to silently ignore a constraint it does not understand —
//! whatever a plugin declares, the kernel executes in full. When a workflow
//! ever needs full JSON Schema, replace this module (single-function seam).
//!
//! Supported subset:
//!   * root: `type: "object"`, `properties`, `required`,
//!     `additionalProperties: false` (must be **present**; a schema is not
//!     allowed to silently carry open-world semantics), `description`;
//!   * per property: `type ∈ {string, integer, number, boolean}`, `enum`
//!     (non-empty array of strings, only with `type: "string"`), `default`
//!     (must itself satisfy the property's type/enum), `description`.

use serde_json::{Map, Value};

/// Byte cap for both the serialized `input_schema` and the serialized
/// `workflow_input` instance — mirrors the `spec_instructions` limit in
/// `WorkflowDescriptor::validate`: both end up injected into the spec
/// prompt, so user-controlled input must stay bounded.
pub const WORKFLOW_INPUT_MAX_BYTES: usize = 8192;

const ROOT_KEYWORDS: [&str; 5] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "description",
];
const PROPERTY_KEYWORDS: [&str; 4] = ["type", "enum", "default", "description"];
const PROPERTY_TYPES: [&str; 4] = ["string", "integer", "number", "boolean"];

/// A schema-subset violation: `path` is relative to the `input_schema` value
/// (e.g. `input_schema.properties.merge_policy.enum`) so the manifest layer
/// can prefix it with `workflows[i].`.
#[derive(Debug)]
pub struct SchemaError {
    pub path: String,
    pub reason: String,
}

impl SchemaError {
    fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        SchemaError {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// Validate that `schema` stays inside the supported subset. Run at
/// manifest-validation time (fail-close at the authoring point).
pub fn validate_input_schema(schema: &Value) -> Result<(), SchemaError> {
    let path = |s: &str| format!("input_schema{s}");

    if serde_json::to_string(schema)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
        > WORKFLOW_INPUT_MAX_BYTES
    {
        return Err(SchemaError::new(
            path(""),
            format!("must serialize to at most {WORKFLOW_INPUT_MAX_BYTES} bytes"),
        ));
    }

    let root = schema
        .as_object()
        .ok_or_else(|| SchemaError::new(path(""), "must be a JSON object"))?;

    for key in root.keys() {
        if !ROOT_KEYWORDS.contains(&key.as_str()) {
            return Err(SchemaError::new(
                path(&format!(".{key}")),
                format!("unsupported keyword `{key}`; supported root keywords: {ROOT_KEYWORDS:?}"),
            ));
        }
    }

    if root.get("type").and_then(Value::as_str) != Some("object") {
        return Err(SchemaError::new(
            path(".type"),
            "must be exactly \"object\"",
        ));
    }

    // `additionalProperties: false` must be explicit — absence would smuggle
    // in JSON Schema's open-world default, which the instance validator
    // (deliberately) does not implement.
    match root.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(_) => {
            return Err(SchemaError::new(
                path(".additionalProperties"),
                "must be exactly false (open-world schemas are not supported)",
            ));
        }
        None => {
            return Err(SchemaError::new(
                path(".additionalProperties"),
                "must be present and false (open-world schemas are not supported)",
            ));
        }
    }

    let empty = Map::new();
    let properties = match root.get("properties") {
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(SchemaError::new(
                path(".properties"),
                "must be a JSON object",
            ));
        }
        None => &empty,
    };

    for (name, spec) in properties {
        validate_property(name, spec).map_err(|e| SchemaError {
            path: path(&format!(".properties.{name}{}", e.path)),
            reason: e.reason,
        })?;
    }

    if let Some(required) = root.get("required") {
        let items = required
            .as_array()
            .ok_or_else(|| SchemaError::new(path(".required"), "must be an array of strings"))?;
        let mut seen = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let key = item.as_str().ok_or_else(|| {
                SchemaError::new(path(&format!(".required[{i}]")), "must be a string")
            })?;
            if !properties.contains_key(key) {
                return Err(SchemaError::new(
                    path(&format!(".required[{i}]")),
                    format!("`{key}` is not declared in properties"),
                ));
            }
            if seen.contains(&key) {
                return Err(SchemaError::new(
                    path(&format!(".required[{i}]")),
                    format!("duplicate required key `{key}`"),
                ));
            }
            seen.push(key);
        }
    }

    Ok(())
}

/// Validate one property spec; error paths are relative to the property
/// (empty string = the property object itself).
fn validate_property(_name: &str, spec: &Value) -> Result<(), SchemaError> {
    let spec = spec
        .as_object()
        .ok_or_else(|| SchemaError::new("", "must be a JSON object"))?;

    for key in spec.keys() {
        if !PROPERTY_KEYWORDS.contains(&key.as_str()) {
            return Err(SchemaError::new(
                format!(".{key}"),
                format!(
                    "unsupported keyword `{key}`; supported property keywords: {PROPERTY_KEYWORDS:?}"
                ),
            ));
        }
    }

    let ty = spec
        .get("type")
        .ok_or_else(|| SchemaError::new(".type", "is required"))?
        .as_str()
        .ok_or_else(|| SchemaError::new(".type", "must be a string"))?;
    if !PROPERTY_TYPES.contains(&ty) {
        return Err(SchemaError::new(
            ".type",
            format!("unsupported type `{ty}`; supported: {PROPERTY_TYPES:?}"),
        ));
    }

    if let Some(members) = spec.get("enum") {
        // v1 subset: string enums only — an enum riding next to
        // `type: "integer"` etc. is declarable-but-unsatisfiable and is
        // rejected outright.
        if ty != "string" {
            return Err(SchemaError::new(
                ".enum",
                format!("enum is only supported with type \"string\" (got type `{ty}`)"),
            ));
        }
        let members = members
            .as_array()
            .ok_or_else(|| SchemaError::new(".enum", "must be a non-empty array of strings"))?;
        if members.is_empty() {
            return Err(SchemaError::new(
                ".enum",
                "must be a non-empty array of strings",
            ));
        }
        if let Some(i) = members.iter().position(|m| !m.is_string()) {
            return Err(SchemaError::new(format!(".enum[{i}]"), "must be a string"));
        }
    }

    if let Some(default) = spec.get("default")
        && let Err(reason) = check_value(default, spec)
    {
        return Err(SchemaError::new(
            ".default",
            format!("default does not satisfy the property's own constraints: {reason}"),
        ));
    }

    Ok(())
}

/// Validate a `workflow_input` instance against an already subset-validated
/// `input_schema`. Errors carry the offending field path
/// (`workflow_input.merge_policy: expected one of […]`) so the route can
/// surface them verbatim in a 400.
pub fn validate_workflow_input(schema: &Value, input: &Value) -> Result<(), String> {
    if serde_json::to_string(input)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
        > WORKFLOW_INPUT_MAX_BYTES
    {
        return Err(format!(
            "workflow_input: must serialize to at most {WORKFLOW_INPUT_MAX_BYTES} bytes"
        ));
    }

    let object = input
        .as_object()
        .ok_or_else(|| "workflow_input: expected a JSON object".to_string())?;

    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                return Err(format!("workflow_input.{key}: required field is missing"));
            }
        }
    }

    for (key, value) in object {
        // `additionalProperties: false` is guaranteed present by the subset
        // validator — undeclared keys are always rejected.
        let Some(spec) = properties.get(key) else {
            return Err(format!(
                "workflow_input.{key}: unknown field (schema declares additionalProperties: false)"
            ));
        };
        check_value(value, spec.as_object().unwrap_or(&empty))
            .map_err(|reason| format!("workflow_input.{key}: {reason}"))?;
    }

    Ok(())
}

/// Check a single value against a property spec's `type` + `enum`.
fn check_value(value: &Value, spec: &Map<String, Value>) -> Result<(), String> {
    let ty = spec.get("type").and_then(Value::as_str).unwrap_or("string");
    let ok = match ty {
        "string" => value.is_string(),
        // JSON has one number type; "integer" additionally rejects
        // fractional values.
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        _ => false,
    };
    if !ok {
        return Err(format!("expected type `{ty}`"));
    }
    if let Some(members) = spec.get("enum").and_then(Value::as_array)
        && !members.contains(value)
    {
        let allowed: Vec<&str> = members.iter().filter_map(Value::as_str).collect();
        return Err(format!("expected one of {allowed:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_url": { "type": "string", "description": "Canonical issue URL" },
                "issue_number": { "type": "integer" },
                "merge_policy": {
                    "type": "string",
                    "enum": ["hold-for-ratify", "auto-merge"],
                    "default": "hold-for-ratify"
                },
                "dry_run": { "type": "boolean" },
                "weight": { "type": "number" }
            },
            "required": ["issue_url", "issue_number"],
            "additionalProperties": false
        })
    }

    // ---------------- subset validator ----------------

    #[test]
    fn accepts_v1_shaped_schema() {
        validate_input_schema(&schema()).expect("subset schema accepted");
    }

    #[test]
    fn accepts_schema_without_properties_or_required() {
        validate_input_schema(&json!({
            "type": "object",
            "additionalProperties": false
        }))
        .expect("minimal closed schema accepted");
    }

    #[test]
    fn rejects_non_object_root_and_wrong_type() {
        let err = validate_input_schema(&json!("nope")).unwrap_err();
        assert_eq!(err.path, "input_schema");

        let err = validate_input_schema(&json!({
            "type": "array",
            "additionalProperties": false
        }))
        .unwrap_err();
        assert_eq!(err.path, "input_schema.type");
    }

    #[test]
    fn rejects_hostile_root_keywords() {
        for keyword in ["$ref", "oneOf", "allOf", "patternProperties", "$defs"] {
            let mut v = schema();
            v[keyword] = json!({});
            let err = validate_input_schema(&v).unwrap_err();
            assert_eq!(err.path, format!("input_schema.{keyword}"), "{keyword}");
        }
    }

    #[test]
    fn rejects_hostile_property_keywords() {
        for keyword in ["format", "pattern", "$ref", "minLength", "items"] {
            let mut v = schema();
            v["properties"]["issue_url"][keyword] = json!("x");
            let err = validate_input_schema(&v).unwrap_err();
            assert_eq!(
                err.path,
                format!("input_schema.properties.issue_url.{keyword}"),
                "{keyword}"
            );
        }
    }

    #[test]
    fn rejects_nested_object_and_array_property_types() {
        for ty in ["object", "array", "null"] {
            let mut v = schema();
            v["properties"]["issue_url"] = json!({ "type": ty });
            let err = validate_input_schema(&v).unwrap_err();
            assert_eq!(err.path, "input_schema.properties.issue_url.type", "{ty}");
        }
    }

    #[test]
    fn rejects_missing_or_non_false_additional_properties() {
        let mut v = schema();
        v.as_object_mut().unwrap().remove("additionalProperties");
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.additionalProperties");

        let mut v = schema();
        v["additionalProperties"] = json!(true);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.additionalProperties");
    }

    #[test]
    fn rejects_required_key_not_in_properties() {
        let mut v = schema();
        v["required"] = json!(["issue_url", "ghost"]);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.required[1]");
        assert!(err.reason.contains("ghost"));
    }

    #[test]
    fn rejects_enum_on_non_string_type() {
        let mut v = schema();
        v["properties"]["issue_number"] = json!({ "type": "integer", "enum": [1, 2] });
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.issue_number.enum");
    }

    #[test]
    fn rejects_empty_or_non_string_enum_members() {
        let mut v = schema();
        v["properties"]["merge_policy"]["enum"] = json!([]);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.merge_policy.enum");

        let mut v = schema();
        v["properties"]["merge_policy"]["enum"] = json!(["ok", 3]);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.merge_policy.enum[1]");
    }

    #[test]
    fn rejects_default_that_violates_own_constraints() {
        // default outside its own enum
        let mut v = schema();
        v["properties"]["merge_policy"]["default"] = json!("yolo-merge");
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.merge_policy.default");

        // default of the wrong type
        let mut v = schema();
        v["properties"]["issue_number"] = json!({ "type": "integer", "default": "42" });
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.issue_number.default");
    }

    #[test]
    fn rejects_oversized_schema() {
        let mut v = schema();
        v["description"] = json!("x".repeat(WORKFLOW_INPUT_MAX_BYTES));
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema");
        assert!(err.reason.contains("8192"));
    }

    // ---------------- instance validator ----------------

    #[test]
    fn accepts_conforming_input() {
        validate_workflow_input(
            &schema(),
            &json!({
                "issue_url": "https://github.com/o/r/issues/1",
                "issue_number": 1,
                "merge_policy": "auto-merge",
                "dry_run": true,
                "weight": 0.5
            }),
        )
        .expect("conforming input accepted");
    }

    #[test]
    fn rejects_missing_required_field() {
        let err = validate_workflow_input(&schema(), &json!({ "issue_url": "u" })).unwrap_err();
        assert!(err.starts_with("workflow_input.issue_number:"), "{err}");
    }

    #[test]
    fn rejects_type_mismatches() {
        let err =
            validate_workflow_input(&schema(), &json!({ "issue_url": "u", "issue_number": "1" }))
                .unwrap_err();
        assert!(err.starts_with("workflow_input.issue_number:"), "{err}");
        assert!(err.contains("integer"), "{err}");

        // fractional value against "integer"
        let err =
            validate_workflow_input(&schema(), &json!({ "issue_url": "u", "issue_number": 1.5 }))
                .unwrap_err();
        assert!(err.starts_with("workflow_input.issue_number:"), "{err}");
    }

    #[test]
    fn rejects_enum_violation_naming_field_and_members() {
        let err = validate_workflow_input(
            &schema(),
            &json!({ "issue_url": "u", "issue_number": 1, "merge_policy": "yolo" }),
        )
        .unwrap_err();
        assert!(err.starts_with("workflow_input.merge_policy:"), "{err}");
        assert!(err.contains("hold-for-ratify"), "{err}");
        assert!(err.contains("auto-merge"), "{err}");
    }

    #[test]
    fn rejects_undeclared_key() {
        let err = validate_workflow_input(
            &schema(),
            &json!({ "issue_url": "u", "issue_number": 1, "ghost": true }),
        )
        .unwrap_err();
        assert!(err.starts_with("workflow_input.ghost:"), "{err}");
    }

    #[test]
    fn rejects_non_object_input() {
        let err = validate_workflow_input(&schema(), &json!(["not", "an", "object"])).unwrap_err();
        assert!(err.contains("expected a JSON object"), "{err}");
    }

    #[test]
    fn rejects_oversized_input() {
        let err = validate_workflow_input(
            &schema(),
            &json!({
                "issue_url": "x".repeat(WORKFLOW_INPUT_MAX_BYTES),
                "issue_number": 1
            }),
        )
        .unwrap_err();
        assert!(err.contains("8192"), "{err}");
    }
}
