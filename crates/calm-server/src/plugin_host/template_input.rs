//! Issue #891 / #1110 S2 — hand-rolled JSON-Schema **subset** for
//! `Manifest.input_schema` and the matching instance validator for
//! `NewTrack.template_input`.
//!
//! **#1284 S1 — second user.** `Manifest.config_schema` (plugin user config)
//! reuses this exact subset and this exact instance validator. The module doc
//! below predicted a second user; what it did not predict is that the error
//! paths were hard-coded to `input_schema…` / `template_input…`, so a
//! `config_schema` violation would have been reported against a field the
//! manifest does not even have. Both validators therefore take a `root_path`
//! now ([`validate_object_schema`] / [`validate_instance`]); the two original
//! entry points are thin wrappers that pass the original literals, which is
//! what keeps every pre-existing error string byte-identical.
//!
//! Deliberately not the `jsonschema` crate (twice-recorded decision:
//! `manifest.rs` module doc + `calm-server/Cargo.toml` dependency notes): the
//! supported surface is a closed keyword set, small enough that hand-written
//! validation gives better error messages without a new dependency tree. The
//! subset is enforced at manifest-validation time so the instance validator
//! below never has to silently ignore a constraint it does not understand —
//! whatever a plugin declares, the kernel executes in full. When a template
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
/// `template_input` instance. Bound input is injected into the planner
/// prompt, so user-controlled JSON must stay bounded.
pub const TEMPLATE_INPUT_MAX_BYTES: usize = 8192;

const ROOT_KEYWORDS: [&str; 5] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "description",
];
const PROPERTY_KEYWORDS: [&str; 4] = ["type", "enum", "default", "description"];
const PROPERTY_TYPES: [&str; 4] = ["string", "integer", "number", "boolean"];

/// A schema-subset violation: `path` is rooted at the Manifest field
/// (e.g. `input_schema.properties.merge_policy.enum`).
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

/// Validate that `schema` stays inside the supported subset, reporting
/// violations under `root_path` (the Manifest field the schema was read from:
/// `input_schema`, `config_schema`, …). Run at manifest-validation time
/// (fail-close at the authoring point).
pub fn validate_object_schema(root_path: &str, schema: &Value) -> Result<(), SchemaError> {
    let path = |s: &str| format!("{root_path}{s}");

    if serde_json::to_string(schema)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
        > TEMPLATE_INPUT_MAX_BYTES
    {
        return Err(SchemaError::new(
            path(""),
            format!("must serialize to at most {TEMPLATE_INPUT_MAX_BYTES} bytes"),
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

    if let Some(description) = root.get("description")
        && !description.is_string()
    {
        return Err(SchemaError::new(path(".description"), "must be a string"));
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

    for (name, property_schema) in properties {
        validate_property(name, property_schema).map_err(|e| SchemaError {
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

/// `Manifest.input_schema`'s entry point — [`validate_object_schema`] rooted
/// at the field name it has always reported.
pub fn validate_input_schema(schema: &Value) -> Result<(), SchemaError> {
    validate_object_schema("input_schema", schema)
}

/// Validate one property schema; error paths are relative to the property
/// (empty string = the property object itself).
fn validate_property(_name: &str, schema: &Value) -> Result<(), SchemaError> {
    let schema = schema
        .as_object()
        .ok_or_else(|| SchemaError::new("", "must be a JSON object"))?;

    for key in schema.keys() {
        if !PROPERTY_KEYWORDS.contains(&key.as_str()) {
            return Err(SchemaError::new(
                format!(".{key}"),
                format!(
                    "unsupported keyword `{key}`; supported property keywords: {PROPERTY_KEYWORDS:?}"
                ),
            ));
        }
    }

    if let Some(description) = schema.get("description")
        && !description.is_string()
    {
        return Err(SchemaError::new(".description", "must be a string"));
    }

    let ty = schema
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

    if let Some(members) = schema.get("enum") {
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

    if let Some(default) = schema.get("default")
        && let Err(reason) = check_value(default, schema)
    {
        return Err(SchemaError::new(
            ".default",
            format!("default does not satisfy the property's own constraints: {reason}"),
        ));
    }

    Ok(())
}

/// Validate an instance against an already subset-validated schema of this
/// module's subset. Errors carry the offending field path rooted at
/// `root_path` (`template_input.merge_policy: expected one of […]`,
/// `config.theme: expected type \`string\``) so the route can surface them
/// verbatim in a 400.
pub fn validate_instance(root_path: &str, schema: &Value, input: &Value) -> Result<(), String> {
    if serde_json::to_string(input)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
        > TEMPLATE_INPUT_MAX_BYTES
    {
        return Err(format!(
            "{root_path}: must serialize to at most {TEMPLATE_INPUT_MAX_BYTES} bytes"
        ));
    }

    let object = input
        .as_object()
        .ok_or_else(|| format!("{root_path}: expected a JSON object"))?;

    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                return Err(format!("{root_path}.{key}: required field is missing"));
            }
        }
    }

    // `additionalProperties: false` is guaranteed present by the subset
    // validator — undeclared keys are always rejected.
    reject_undeclared_keys(root_path, schema, object.keys().map(String::as_str))?;

    for (key, value) in object {
        // Unreachable-by-construction: the sweep above refused every key that
        // is not in `properties`. Written as a `?` rather than an `unwrap` so a
        // future regression up there fails closed instead of panicking.
        let schema = properties
            .get(key)
            .ok_or_else(|| undeclared_key_error(root_path, key))?;
        check_value(value, schema.as_object().unwrap_or(&empty))
            .map_err(|reason| format!("{root_path}.{key}: {reason}"))?;
    }

    Ok(())
}

fn undeclared_key_error(root_path: &str, key: &str) -> String {
    format!("{root_path}.{key}: unknown field (schema declares additionalProperties: false)")
}

/// Reject any key `schema.properties` does not declare — the *same* rule
/// [`validate_instance`] applies, exposed on its own.
///
/// #1284 S1 review: the plugin-config `PATCH` has to judge the **request
/// document's** key names before it can interpret their values, because an
/// explicit `null` there means "delete this key" and would otherwise vanish
/// from the merged map before any validation saw it — `{"ghost": null}`
/// returned 200 against a schema that declares no `ghost`. The route calls
/// this function rather than restating the rule, so "which keys does this
/// schema declare" has exactly one implementation and one error string.
pub fn reject_undeclared_keys<'a>(
    root_path: &str,
    schema: &Value,
    keys: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    for key in keys {
        if !declares_key(schema, key) {
            return Err(undeclared_key_error(root_path, key));
        }
    }
    Ok(())
}

/// Does `schema` declare `key`? The predicate underneath
/// [`reject_undeclared_keys`], as a plain boolean.
///
/// #1284 S1 review P2-G. Callers that need a *filter* rather than a *verdict*
/// (the config PATCH prunes stored residue with one) were reading
/// `reject_undeclared_keys(..).is_ok()`, i.e. using a `Result` as a boolean.
/// That is fine only for as long as the function has exactly one failure mode:
/// the day it grows a second — a malformed schema, a reserved key name — every
/// such call site would silently reclassify that new failure as "not
/// declared", and the pruning one would translate it into deleting the
/// operator's data. A `bool` cannot acquire a second failure mode.
pub fn declares_key(schema: &Value, key: &str) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(key))
}

/// `NewTrack.template_input`'s entry point — [`validate_instance`] rooted at
/// the field name it has always reported.
pub fn validate_template_input(schema: &Value, input: &Value) -> Result<(), String> {
    validate_instance("template_input", schema, input)
}

/// Why a `template_input` has no owning plugin Manifest to be checked against
/// — or the Manifest itself.
///
/// # #1321 S1 第二轮评审 NIT-3
///
/// This used to be a bare `Option<&Manifest>`, and the `None` arm answered
/// "``template_input`` requires ``template_id``" for **both** of its causes.
/// The second cause makes that message a lie, and it is reachable: `create_track`
/// derives its Manifest from `admit_template(..).binding`, which is `None`
/// whenever the roster admits the `template_id` but no *running ∧ trusted*
/// plugin currently declares it (`routes::tracks::resolve_template_binding`).
/// A caller who sent a perfectly good `template_id` while the owning plugin was
/// stopped was told to supply the field they had already supplied.
///
/// Splitting the `None` lets each cause say what actually went wrong, and makes
/// the "whole matrix" claim on [`validate_template_input_binding`] true of the
/// owner axis as well as the input axis.
pub enum TemplateInputOwner<'a> {
    /// No `template_id` was given at all, so there is no plugin to bind to.
    NoTemplateId,
    /// A `template_id` was given and the roster admits it, but no running ∧
    /// trusted plugin declares it right now, so there is no `input_schema` to
    /// check the input against.
    NoBoundPlugin,
    /// The owning plugin's current Manifest.
    Plugin(&'a crate::plugin_host::manifest::Manifest),
}

/// #891 / #1110 S2 — the **whole** `(owner, template_input)` matrix,
/// fail-closed: input is only accepted when a bound plugin Manifest declares an
/// `input_schema`, and a schema with required fields makes input mandatory. The
/// kernel never applies schema `default`s — the value persists exactly as the
/// caller sent it. Descriptor-level `input_schema` is never consulted.
///
/// # #1321 S1 — why this lives here and not in `routes::tracks`
///
/// It used to be a private fn in `routes::tracks`, and the run-time re-check
/// in [`crate::track_binding`] restated *half* of it (only the
/// `(Some(schema), Some(input))` arm). The two therefore disagreed on the
/// same triple: a track created while its owner declared no `input_schema`
/// stores `template_input = NULL`, and after an owner upgrade that adds
/// `required: [...]` the create route answers 400 for that exact
/// (plugin, template, input) while the run-time restatement answered "fine".
/// CLAUDE.md「Mirror Code Must Call The Original」: the restatement is gone
/// and both callers now enter *this* function.
///
/// The error is a bare reason with no route vocabulary in it —
/// `routes::tracks::create_track` prefixes `track create: ` and the binding
/// resolver reports it as a contract failure. The prefix keeps the
/// pre-existing 400 bodies byte-identical **except** for the
/// [`TemplateInputOwner::NoBoundPlugin`] + `Some(input)` cell, which #1321 S1
/// deliberately changed: it used to answer "`template_input` requires
/// `template_id`" — asking the caller for the field they had just sent — and
/// now names the real cause (no running ∧ trusted plugin declares the
/// template). That single reworded body is pinned by
/// `routes::tracks::tests::template_input_binding::input_with_a_template_whose_owner_is_not_running_names_that_cause`
/// and, at the HTTP boundary, by
/// `track_binding::tests::create_time_and_run_time_binding_agree_for_a_stopped_owner`.
pub fn validate_template_input_binding(
    owner: TemplateInputOwner<'_>,
    input: Option<&Value>,
) -> Result<(), String> {
    let plugin = match owner {
        TemplateInputOwner::Plugin(plugin) => plugin,
        TemplateInputOwner::NoTemplateId => {
            if input.is_some() {
                return Err("`template_input` requires `template_id`".into());
            }
            return Ok(());
        }
        TemplateInputOwner::NoBoundPlugin => {
            if input.is_some() {
                return Err(
                    "`template_input` requires a `template_id` whose owning plugin is \
                     currently running and trusted; no running and trusted plugin declares \
                     this template right now, so there is no input_schema to validate against"
                        .into(),
                );
            }
            return Ok(());
        }
    };
    let plugin_id = &plugin.id;
    match (plugin.input_schema.as_ref(), input) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(format!(
            "plugin `{plugin_id}` does not declare an input_schema; \
             `template_input` is not accepted"
        )),
        (Some(schema), None) => {
            let required: Vec<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|keys| keys.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if required.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "plugin `{plugin_id}` requires `template_input` \
                     (required: {required:?})"
                ))
            }
        }
        (Some(schema), Some(input)) => validate_template_input(schema, input),
    }
}

/// Check a single value against a property schema's `type` + `enum`.
fn check_value(value: &Value, schema: &Map<String, Value>) -> Result<(), String> {
    let ty = schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string");
    let ok = match ty {
        "string" => value.is_string(),
        // Deliberate deviation from JSON Schema `integer` semantics
        // (design doc §决策记录): only integer-*encoded* JSON numbers are
        // accepted — ANY float-encoded value is rejected, including `1.0`,
        // not just fractional ones. Fail-closed: re-deriving integrality
        // from an f64 hits precision edge cases (`1e300`, values past
        // 2^53). Conforming clients are unaffected — JS
        // `JSON.stringify(1.0)` emits `"1"`.
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        _ => false,
    };
    if !ok {
        if ty == "integer" {
            return Err("expected type `integer` (an integer-encoded JSON number; \
                 float-encoded values such as `1.0` are rejected)"
                .to_string());
        }
        return Err(format!("expected type `{ty}`"));
    }
    if let Some(members) = schema.get("enum").and_then(Value::as_array)
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
    fn rejects_non_string_root_description() {
        let mut v = schema();
        v["description"] = json!(false);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.description");

        let mut v = schema();
        v["description"] = json!({ "oneOf": [] });
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.description");
    }

    #[test]
    fn rejects_non_string_property_description() {
        let mut v = schema();
        v["properties"]["issue_url"]["description"] = json!(false);
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.issue_url.description");

        let mut v = schema();
        v["properties"]["issue_url"]["description"] = json!({ "oneOf": [] });
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema.properties.issue_url.description");
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
        v["description"] = json!("x".repeat(TEMPLATE_INPUT_MAX_BYTES));
        let err = validate_input_schema(&v).unwrap_err();
        assert_eq!(err.path, "input_schema");
        assert!(err.reason.contains("8192"));
    }

    // ---------------- instance validator ----------------

    #[test]
    fn accepts_conforming_input() {
        validate_template_input(
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
        let err = validate_template_input(&schema(), &json!({ "issue_url": "u" })).unwrap_err();
        assert!(err.starts_with("template_input.issue_number:"), "{err}");
    }

    #[test]
    fn rejects_type_mismatches() {
        let err =
            validate_template_input(&schema(), &json!({ "issue_url": "u", "issue_number": "1" }))
                .unwrap_err();
        assert!(err.starts_with("template_input.issue_number:"), "{err}");
        assert!(err.contains("integer"), "{err}");

        // fractional value against "integer"
        let err =
            validate_template_input(&schema(), &json!({ "issue_url": "u", "issue_number": 1.5 }))
                .unwrap_err();
        assert!(err.starts_with("template_input.issue_number:"), "{err}");
    }

    #[test]
    fn integer_accepts_integer_encoded_value() {
        validate_template_input(&schema(), &json!({ "issue_url": "u", "issue_number": 1 }))
            .expect("integer-encoded 1 accepted");
    }

    #[test]
    fn integer_rejects_float_encoded_value_even_when_whole() {
        // Deliberate strictness (see check_value): `1.0` is float-encoded,
        // so it is rejected even though it is numerically integral.
        let err =
            validate_template_input(&schema(), &json!({ "issue_url": "u", "issue_number": 1.0 }))
                .unwrap_err();
        assert!(err.starts_with("template_input.issue_number:"), "{err}");
        assert!(err.contains("float-encoded"), "{err}");
    }

    #[test]
    fn rejects_enum_violation_naming_field_and_members() {
        let err = validate_template_input(
            &schema(),
            &json!({ "issue_url": "u", "issue_number": 1, "merge_policy": "yolo" }),
        )
        .unwrap_err();
        assert!(err.starts_with("template_input.merge_policy:"), "{err}");
        assert!(err.contains("hold-for-ratify"), "{err}");
        assert!(err.contains("auto-merge"), "{err}");
    }

    #[test]
    fn rejects_undeclared_key() {
        let err = validate_template_input(
            &schema(),
            &json!({ "issue_url": "u", "issue_number": 1, "ghost": true }),
        )
        .unwrap_err();
        assert!(err.starts_with("template_input.ghost:"), "{err}");
    }

    #[test]
    fn rejects_non_object_input() {
        let err = validate_template_input(&schema(), &json!(["not", "an", "object"])).unwrap_err();
        assert!(err.contains("expected a JSON object"), "{err}");
    }

    // ---------------- root-path parameterization (#1284 S1) ----------------
    //
    // The two wrappers above must keep reporting `input_schema…` /
    // `template_input…` (pinned by every assertion in this file), and an
    // arbitrary root must be honoured verbatim so `config_schema` violations
    // do not get reported against `input_schema`.

    #[test]
    fn schema_violations_report_under_the_callers_root_path() {
        let mut v = schema();
        v["properties"]["merge_policy"]["default"] = json!("yolo-merge");

        // positive control: the wrapper's root is unchanged
        assert_eq!(
            validate_input_schema(&v).unwrap_err().path,
            "input_schema.properties.merge_policy.default"
        );
        // and the parameterized form reports the root it was given
        assert_eq!(
            validate_object_schema("config_schema", &v)
                .unwrap_err()
                .path,
            "config_schema.properties.merge_policy.default"
        );
    }

    #[test]
    fn instance_violations_report_under_the_callers_root_path() {
        let bad = json!({ "issue_url": "u", "issue_number": "1" });

        assert!(
            validate_template_input(&schema(), &bad)
                .unwrap_err()
                .starts_with("template_input.issue_number:")
        );
        assert!(
            validate_instance("config", &schema(), &bad)
                .unwrap_err()
                .starts_with("config.issue_number:")
        );

        // The non-object and byte-cap arms are rooted too — they are the two
        // that name the root with no field suffix. Both are asserted, because
        // they are two separate `format!`s and the comment used to claim
        // coverage the test did not have.
        assert!(
            validate_instance("config", &schema(), &json!([]))
                .unwrap_err()
                .starts_with("config: ")
        );
        let oversized = json!({
            "issue_url": "x".repeat(TEMPLATE_INPUT_MAX_BYTES),
            "issue_number": 1
        });
        let err = validate_instance("config", &schema(), &oversized).unwrap_err();
        assert!(err.starts_with("config: "), "{err}");
        assert!(err.contains("8192"), "{err}");
    }

    /// #1284 S1 review — the string an undeclared key produces, pinned at both
    /// entry points.
    ///
    /// **Round 2 (P2-H):** this test used to compare the two errors to each
    /// other (`assert_eq!(inline, extracted)`), which is a tautology —
    /// `validate_instance` *calls* `reject_undeclared_keys`, so the expression
    /// expands to `f(x) == f(x)` and cannot fail for any implementation. It is
    /// the same shape as the `both_entry_points_agree` assertion round 1
    /// deleted. What has content is the literal on the right-hand side: it is
    /// the text that ships in a 400 body, so both entry points are asserted
    /// against the literal instead of against each other.
    #[test]
    fn an_undeclared_key_reports_the_same_shipped_string_at_both_entry_points() {
        let inline = validate_instance(
            "config",
            &schema(),
            &json!({ "issue_url": "u", "issue_number": 1, "ghost": true }),
        )
        .unwrap_err();
        assert_eq!(
            inline,
            "config.ghost: unknown field (schema declares additionalProperties: false)"
        );
        let extracted =
            reject_undeclared_keys("config", &schema(), ["issue_url", "ghost"].into_iter())
                .unwrap_err();
        assert_eq!(
            extracted,
            "config.ghost: unknown field (schema declares additionalProperties: false)"
        );

        // …and it accepts what the schema does declare, so the check is not a
        // constant `Err`.
        reject_undeclared_keys("config", &schema(), ["issue_url", "dry_run"].into_iter())
            .expect("declared keys pass");
    }

    /// A schema with no `properties` at all declares **nothing**, so every key
    /// is undeclared. (The subset validator accepts such a schema — see
    /// `accepts_schema_without_properties_or_required`.)
    #[test]
    fn reject_undeclared_keys_refuses_everything_when_properties_is_absent() {
        let closed = json!({ "type": "object", "additionalProperties": false });
        assert!(reject_undeclared_keys("config", &closed, ["x"].into_iter()).is_err());
        reject_undeclared_keys("config", &closed, std::iter::empty()).expect("no keys, no verdict");
        assert!(!declares_key(&closed, "x"));
    }

    /// P2-G — the boolean the config PATCH prunes with, in both directions,
    /// so it is not a constant.
    #[test]
    fn declares_key_answers_the_membership_question_directly() {
        assert!(declares_key(&schema(), "issue_url"));
        assert!(!declares_key(&schema(), "ghost"));
    }

    #[test]
    fn rejects_oversized_input() {
        let err = validate_template_input(
            &schema(),
            &json!({
                "issue_url": "x".repeat(TEMPLATE_INPUT_MAX_BYTES),
                "issue_number": 1
            }),
        )
        .unwrap_err();
        assert!(err.contains("8192"), "{err}");
    }
}
