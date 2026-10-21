use serde_json::Value;

/// Evaluates the JSON Schema keywords the published task schema uses. A keyword outside that set
/// panics instead of passing, so the evaluator cannot silently accept what it does not check.
pub(super) fn accepts(schema: &Value, value: &Value, root: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return schema
            .as_bool()
            .expect("a schema is an object or a boolean");
    };
    schema.iter().all(|(keyword, rule)| match keyword.as_str() {
        "description" | "default" | "examples" | "title" | "$defs" | "$schema" => true,
        "$ref" => {
            let name = rule
                .as_str()
                .and_then(|path| path.strip_prefix("#/$defs/"))
                .expect("a local $defs reference");
            accepts(&root["$defs"][name], value, root)
        }
        "type" => match rule {
            Value::Array(types) => types.iter().any(|kind| is_type(kind, value)),
            kind => is_type(kind, value),
        },
        "enum" => rule.as_array().expect("enum array").contains(value),
        "const" => rule == value,
        "not" => !accepts(rule, value, root),
        "anyOf" => branches(rule).any(|branch| accepts(branch, value, root)),
        "allOf" => branches(rule).all(|branch| accepts(branch, value, root)),
        "oneOf" => branches(rule).filter(|b| accepts(b, value, root)).count() == 1,
        "required" => value.as_object().is_none_or(|object| {
            rule.as_array()
                .expect("required array")
                .iter()
                .all(|field| object.contains_key(field.as_str().expect("field name")))
        }),
        "properties" => value.as_object().is_none_or(|object| {
            object.iter().all(|(field, child)| {
                rule.get(field)
                    .is_none_or(|schema| accepts(schema, child, root))
            })
        }),
        "additionalProperties" => value.as_object().is_none_or(|object| {
            let declared = schema.get("properties").and_then(Value::as_object);
            object
                .iter()
                .filter(|(field, _)| declared.is_none_or(|declared| !declared.contains_key(*field)))
                .all(|(_, child)| accepts(rule, child, root))
        }),
        "items" => value
            .as_array()
            .is_none_or(|items| items.iter().all(|item| accepts(rule, item, root))),
        "minimum" | "maximum" => value.as_f64().is_none_or(|number| {
            let bound = rule.as_f64().expect("number bound");
            if keyword == "minimum" {
                number >= bound
            } else {
                number <= bound
            }
        }),
        "minItems" => value
            .as_array()
            .is_none_or(|items| items.len() >= rule.as_u64().unwrap() as usize),
        "minLength" | "maxLength" | "pattern" => value.as_str().is_none_or(|text| {
            let chars = text.chars().count() as u64;
            match keyword.as_str() {
                "minLength" => chars >= rule.as_u64().expect("minLength"),
                "maxLength" => chars <= rule.as_u64().expect("maxLength"),
                _ => regex::Regex::new(rule.as_str().expect("pattern"))
                    .expect("a pattern the evaluator can compile")
                    .is_match(text),
            }
        }),
        other => panic!("the parity evaluator does not implement keyword {other:?}"),
    })
}

fn branches(rule: &Value) -> impl Iterator<Item = &Value> {
    rule.as_array().expect("combinator array").iter()
}

fn is_type(kind: &Value, value: &Value) -> bool {
    match kind.as_str().expect("type name") {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        other => panic!("unknown JSON Schema type {other:?}"),
    }
}
