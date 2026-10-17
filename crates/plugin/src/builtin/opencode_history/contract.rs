//! Projection and validator for the fixed official v1.19.3 schema, not inferred storage.
use super::{Result, ensure};
use serde_json::{Value, json};
use std::sync::LazyLock;
pub const VERSION: &str = "1.19.3";
pub const COMMIT: &str = "e3c4eb242585dcd77be1a867391b7c25fd47ce52";
pub static DOC: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("contract.json")).expect("pinned official projection")
});
fn clean(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "description"
                            | "summary"
                            | "title"
                            | "examples"
                            | "example"
                            | "x-codeSamples"
                    )
                })
                .map(|(k, v)| {
                    let v = match (k.as_str(), v.as_object()) {
                        ("properties", Some(properties)) => Value::Object(
                            properties
                                .iter()
                                .map(|(k, v)| (k.clone(), clean(v)))
                                .collect(),
                        ),
                        _ => clean(v),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(clean).collect()),
        _ => v.clone(),
    }
}
pub fn check_doc(doc: &Value) -> Result<()> {
    let mut paths = serde_json::Map::new();
    for route in DOC["paths"].as_object().expect("paths").keys() {
        let get = &doc["paths"][route]["get"];
        ensure(
            get["operationId"].is_string()
                && get["parameters"].is_array()
                && get["responses"]["200"]["content"]["application/json"]["schema"].is_object(),
            "contract: unsupported OpenAPI",
        )?;
        paths.insert(route.clone(),json!({"get":{"operationId":get["operationId"],"parameters":clean(&get["parameters"]),"responses":{"200":{"content":{"application/json":{"schema":clean(&get["responses"]["200"]["content"]["application/json"]["schema"])}}}}}}));
    }
    let paths = Value::Object(paths);
    let mut schemas = serde_json::Map::new();
    // The shipped projection defines the complete dependency closure. Compare its
    // schemas directly; never traverse arbitrary reference graphs from the server.
    for name in DOC["components"]["schemas"]
        .as_object()
        .expect("schemas")
        .keys()
    {
        let schema = doc["components"]["schemas"]
            .get(name)
            .ok_or("contract: missing schema")?;
        schemas.insert(name.clone(), clean(schema));
    }
    ensure(
        json!({"openapi":doc["openapi"],"paths":paths,"components":{"schemas":schemas}}) == *DOC,
        "contract: OpenAPI mismatch",
    )
}
pub fn named(v: &Value, name: &str) -> Result<()> {
    validate(v, &DOC["components"]["schemas"][name])
}
pub fn validate(v: &Value, s: &Value) -> Result<()> {
    if let Some(r) = s["$ref"].as_str() {
        return named(
            v,
            r.strip_prefix("#/components/schemas/")
                .ok_or("schema: reference")?,
        );
    }
    if let Some(a) = s["anyOf"].as_array() {
        return ensure(
            a.iter().any(|s| validate(v, s).is_ok()),
            "schema: union mismatch",
        );
    }
    ensure(
        match s["type"].as_str() {
            None => true,
            Some("object") => v.is_object(),
            Some("array") => v.is_array(),
            Some("string") => v.is_string(),
            Some("boolean") => v.is_boolean(),
            Some("number") => v.is_number(),
            Some("integer") => v.is_i64() || v.is_u64(),
            Some("null") => v.is_null(),
            _ => false,
        },
        "schema: type mismatch",
    )?;
    if let Some(a) = s["enum"].as_array() {
        ensure(a.contains(v), "schema: enum mismatch")?;
    }
    for (key, lower) in [("minimum", true), ("maximum", false)] {
        if let Some(b) = s[key].as_f64() {
            let n = v.as_f64().ok_or("schema: numeric bound")?;
            ensure(if lower { n >= b } else { n <= b }, "schema: numeric bound")?;
        }
    }
    if let Some(text) = v.as_str()
        && let Some(p) = s["pattern"].as_str()
    {
        // Every pattern in the pinned contract is a literal anchored ID prefix.
        let prefix = p
            .strip_prefix('^')
            .filter(|p| p.chars().all(|c| c.is_ascii_alphabetic()))
            .ok_or("schema: unsupported pattern")?;
        ensure(text.starts_with(prefix), "schema: string pattern")?;
    }
    if let Some(obj) = v.as_object() {
        if let Some(req) = s["required"].as_array() {
            ensure(
                req.iter()
                    .all(|k| k.as_str().is_some_and(|k| obj.contains_key(k))),
                "schema: missing fields",
            )?;
        }
        for (k, v) in obj {
            if let Some(s) = s["properties"].get(k) {
                validate(v, s)?;
            } else if s["additionalProperties"] == false {
                return Err("schema: unknown field".into());
            } else if s["additionalProperties"].is_object() {
                validate(v, &s["additionalProperties"])?;
            }
        }
    }
    if let Some(a) = v.as_array() {
        for v in a {
            validate(v, &s["items"])?;
        }
    }
    Ok(())
}
