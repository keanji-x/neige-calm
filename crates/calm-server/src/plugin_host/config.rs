//! Effective plugin configuration: `defaults ⊕ user_config`, composed on read and never written back, through the one function every consumer goes through.

use serde_json::{Map, Value};

use super::manifest::Manifest;

/// `defaults ⊕ user_config` — what the plugin actually runs with. A user value overrides the default; an unset key with no `default` is absent (never `null`); keys the schema does not declare are dropped; no `config_schema` ⇒ empty map.
/// Defaults are not persisted: materializing them at write time would freeze the manifest's defaults at the operator's first Save and lose "the operator chose this" vs "the manifest said so that day".
pub fn effective_config(manifest: &Manifest, user_config: &Value) -> Map<String, Value> {
    effective_config_from_schema(manifest.config_schema.as_ref(), user_config)
}

/// The merge itself, over a bare schema. Private on purpose: a seam with two doors is not a seam, so [`effective_config`] is the only way in.
fn effective_config_from_schema(
    config_schema: Option<&Value>,
    user_config: &Value,
) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(schema) = config_schema else {
        return out;
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return out;
    };
    let user = user_config.as_object();

    for (key, property_schema) in properties {
        if let Some(value) = user.and_then(|u| u.get(key)) {
            // A stored `null` cannot occur (the write path deletes on `null`), but a hand-edited row could hold one; "no value" falls back to the default.
            if !value.is_null() {
                out.insert(key.clone(), value.clone());
                continue;
            }
        }
        if let Some(default) = property_schema.get("default") {
            out.insert(key.clone(), default.clone());
        }
    }
    out
}

/// Which `config_schema.required` keys are **not** in force. Takes the **effective** map: a key satisfied by its manifest `default` is not missing. Returns the keys in schema-declared order so a composed `last_error` is stable across bring-ups.
/// Empty for a plugin with no `config_schema` or no `required`. Render the refusal with [`missing_required_reason`], not by formatting the list again.
pub fn missing_required(manifest: &Manifest, effective: &Map<String, Value>) -> Vec<String> {
    let Some(schema) = manifest.config_schema.as_ref() else {
        return Vec::new();
    };
    let Some(required) = schema.get("required").and_then(Value::as_array) else {
        return Vec::new();
    };
    required
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| !effective.contains_key(*key))
        .map(String::from)
        .collect()
}

/// The `last_error` wording for [`missing_required`]'s output, as a function so every consumer says the same thing. The instruction is part of the message on purpose: `Unavailable` is terminal (no supervisor, no retry), and "what do I do now" is not available anywhere else in the UI.
pub fn missing_required_reason(missing: &[String]) -> String {
    format!(
        "missing required configuration: {}. Set it under Settings › Plugins, \
         then start the plugin again.",
        missing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::Manifest;
    use serde_json::json;

    /// Drive the real parser, not a hand-built `Manifest`: the schema has to survive `Manifest::validate` for these merges to mean anything.
    fn manifest_with(config_schema: Value, manifest_version: u32) -> Manifest {
        let text = serde_json::to_string(&json!({
            "manifest_version": manifest_version,
            "id": "test.cfg",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Cfg",
            "entrypoint": { "command": "bin/stub" },
            "config_schema": config_schema,
        }))
        .unwrap();
        Manifest::parse(&text).expect("fixture manifest is valid")
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "theme": { "type": "string", "default": "dark" },
                "retries": { "type": "integer", "default": 3 },
                "label": { "type": "string" }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn defaults_fill_keys_the_operator_never_set() {
        let m = manifest_with(schema(), 2);
        let eff = effective_config(&m, &json!({}));
        assert_eq!(eff.get("theme"), Some(&json!("dark")));
        assert_eq!(eff.get("retries"), Some(&json!(3)));
        // no default, not set by the user ⇒ absent, not null
        assert!(!eff.contains_key("label"), "got {eff:?}");
    }

    #[test]
    fn user_values_override_defaults() {
        let m = manifest_with(schema(), 2);
        let eff = effective_config(&m, &json!({ "theme": "light", "label": "x" }));
        assert_eq!(eff.get("theme"), Some(&json!("light")));
        assert_eq!(eff.get("label"), Some(&json!("x")));
        assert_eq!(eff.get("retries"), Some(&json!(3)), "untouched default");
    }

    #[test]
    fn a_cleared_key_falls_back_to_its_default() {
        let m = manifest_with(schema(), 2);
        let configured = effective_config(&m, &json!({ "theme": "light" }));
        assert_eq!(configured.get("theme"), Some(&json!("light")));

        // …and after the delete (the stored map no longer has the key)
        let cleared = effective_config(&m, &json!({}));
        assert_eq!(cleared.get("theme"), Some(&json!("dark")));
    }

    #[test]
    fn keys_the_schema_no_longer_declares_are_dropped() {
        let m = manifest_with(schema(), 2);
        let eff = effective_config(&m, &json!({ "removed_last_version": "residue" }));
        assert!(!eff.contains_key("removed_last_version"), "got {eff:?}");
    }

    #[test]
    fn no_config_schema_yields_an_empty_map() {
        let text = serde_json::to_string(&json!({
            "manifest_version": 1,
            "id": "test.plain",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Plain",
            "entrypoint": { "command": "bin/stub" },
        }))
        .unwrap();
        let m = Manifest::parse(&text).expect("fixture manifest is valid");
        assert!(m.config_schema.is_none());
        assert!(effective_config(&m, &json!({ "theme": "light" })).is_empty());
    }

    /// A schema may legally omit `properties` entirely (the subset validator accepts `{type, additionalProperties}`).
    #[test]
    fn a_schema_without_properties_declares_nothing() {
        let m = manifest_with(
            json!({ "type": "object", "additionalProperties": false }),
            2,
        );
        assert!(
            effective_config(&m, &json!({ "theme": "light" })).is_empty(),
            "a schema with no properties declares no keys"
        );
    }

    /// The write path never stores a `null` (it deletes on `null`), which is why this branch needs its own witness rather than riding on a route test.
    #[test]
    fn a_stored_null_falls_back_to_the_default() {
        let m = manifest_with(schema(), 2);
        let eff = effective_config(&m, &json!({ "theme": null, "label": null }));
        assert_eq!(eff.get("theme"), Some(&json!("dark")), "got {eff:?}");
        assert!(
            !eff.contains_key("label"),
            "no default ⇒ still absent, not null: {eff:?}"
        );
    }

    #[test]
    fn missing_required_names_only_the_keys_nothing_supplies() {
        let m = manifest_with(
            json!({
                "type": "object",
                "properties": {
                    "token": { "type": "string" },
                    "secondary": { "type": "string" },
                    "region": { "type": "string", "default": "eu" }
                },
                "required": ["token", "secondary", "region"],
                "additionalProperties": false
            }),
            3,
        );

        // Nothing set: the defaulted key is in force, the other two are not.
        let eff = effective_config(&m, &json!({}));
        assert_eq!(
            missing_required(&m, &eff),
            vec!["token".to_string(), "secondary".to_string()],
            "declared order, and `region` is satisfied by its default"
        );

        // The operator fills one in…
        let eff = effective_config(&m, &json!({ "token": "t" }));
        assert_eq!(missing_required(&m, &eff), vec!["secondary".to_string()]);

        // …and both: nothing missing, so the plugin may come up.
        let eff = effective_config(&m, &json!({ "token": "t", "secondary": "s" }));
        assert!(missing_required(&m, &eff).is_empty(), "got {eff:?}");
    }

    #[test]
    fn missing_required_is_empty_when_the_manifest_demands_nothing() {
        let no_required = manifest_with(schema(), 2);
        assert!(missing_required(&no_required, &Map::new()).is_empty());

        let text = serde_json::to_string(&json!({
            "manifest_version": 1,
            "id": "test.plain",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Plain",
            "entrypoint": { "command": "bin/stub" },
        }))
        .unwrap();
        let no_schema = Manifest::parse(&text).unwrap();
        assert!(missing_required(&no_schema, &Map::new()).is_empty());
    }

    /// The instruction half is asserted explicitly: without it the message tells an operator what is wrong and not what to do about it.
    #[test]
    fn the_missing_required_reason_names_the_keys_and_the_next_step() {
        let reason = missing_required_reason(&["token".to_string(), "secondary".to_string()]);
        assert_eq!(
            reason,
            "missing required configuration: token, secondary. Set it under \
             Settings › Plugins, then start the plugin again."
        );
    }

    /// The read side degrades a non-object `user_config` to defaults; the write side refuses outright, which is the half that could lose data.
    #[test]
    fn a_non_object_user_config_reads_as_defaults_only() {
        let m = manifest_with(schema(), 2);
        assert_eq!(
            effective_config(&m, &json!("not an object")),
            effective_config(&m, &json!({}))
        );
    }
}
