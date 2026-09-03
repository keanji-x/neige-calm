//! #1284 §2.3 — **effective plugin configuration**, and the single function
//! every consumer of it goes through.
//!
//! The stored value (`plugins.user_config`) is only half of what a plugin
//! actually runs with: the other half is the `default` on each
//! `Manifest.config_schema` property. The composition is
//! `defaults ⊕ user_config`, and it is applied **on read**, never written
//! back — see [`effective_config`]'s own doc for why that direction is
//! load-bearing rather than an implementation detail.
//!
//! **This module exists to be a seam, not to hold an algorithm.** Three
//! separate slices consume configuration (S2 `app` via
//! `initialize._meta["dev.neige/config"]`, S3a `cli-query` via argv slots +
//! `config_env`, S3b `mcp-http` via url slots), plus the two route-side
//! readers here in S1. Five hand-written `⊕`s would be five chances for
//! "default applied" to mean five slightly different things; one function
//! makes "which defaults are in force" a question with exactly one answer.

use serde_json::{Map, Value};

use super::manifest::Manifest;

/// `defaults ⊕ user_config` — what the plugin actually runs with.
///
/// * A schema property with a `default` and no user value contributes its
///   default.
/// * A user value overrides the default for that key.
/// * A key the user never set and whose property has no `default` is simply
///   absent — there is no `null` filler, because `null` is not a value this
///   subset's `type` keyword can ever accept.
/// * Keys in `user_config` that the schema does not declare are dropped. They
///   can only be residue: the write path refuses undeclared keys
///   (`additionalProperties: false`), so the sole way to hold one is to have
///   been configured under an older manifest whose schema has since narrowed.
///   Passing such a key on to a consumer would resurrect a setting the current
///   manifest says does not exist.
/// * No `config_schema` ⇒ empty map, whatever `user_config` holds. Same
///   reason: without a schema there is nothing the kernel can vouch for, and
///   the write path refuses to add anything (400).
///
/// **Defaults are not persisted.** Materializing them into
/// `plugins.user_config` at write time would freeze the manifest's defaults at
/// the moment of the operator's first Save: a later manifest that changes a
/// default would then be permanently invisible to every already-configured
/// install, and the DB would no longer distinguish "the operator chose this"
/// from "this is what the manifest happened to say that day". Reading them
/// keeps the manifest the authority and the DB the record of intent.
pub fn effective_config(manifest: &Manifest, user_config: &Value) -> Map<String, Value> {
    effective_config_from_schema(manifest.config_schema.as_ref(), user_config)
}

/// The merge itself, over a bare schema.
///
/// **Private on purpose (#1284 S1 review).** It used to be `pub` so the routes
/// could merge against the persisted `plugins.manifest` blob; that second
/// entry point was the seam's undoing — a seam with two doors is not a seam,
/// and the test that was supposed to hold them together (`both_entry_points_agree`)
/// compared `f(x)` with `f(x)` and could not fail. The routes now hold a typed
/// [`Manifest`] from the registry like every other consumer, so [`effective_config`]
/// is the only way in.
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

    for (key, spec) in properties {
        if let Some(value) = user.and_then(|u| u.get(key)) {
            // A stored `null` cannot occur — the write path deletes on `null`
            // rather than storing it — but a hand-edited row could hold one,
            // and the honest reading of "no value" is "fall back to default".
            if !value.is_null() {
                out.insert(key.clone(), value.clone());
                continue;
            }
        }
        if let Some(default) = spec.get("default") {
            out.insert(key.clone(), default.clone());
        }
    }
    out
}

/// Which `config_schema.required` keys are **not** in force — the consumption-
/// side half of the §2.2 adjudication, as a carrier rather than a promise.
///
/// #1284 v6 moved `required` off the write path (a Save carries only the keys
/// the operator edited, so enforcing it there makes the first Save of a
/// two-required-key plugin unconditionally 400) and onto consumption: a plugin
/// missing required configuration does not come up, and lands in the
/// `unavailable` + `last_error` terminal state §2.4 defines. S1 owns the write
/// side of that trade, so it also owns the seam the other side needs —
/// otherwise S2, S3a and S3b each write their own "which required keys are
/// missing", which is the restatement §2.3's `effective_config` exists to
/// prevent, one field over.
///
/// Takes the **effective** map, not the stored one: a key satisfied by its
/// manifest `default` is not missing, and only the merged view knows that.
/// Returns the offending keys in schema-declared order so the `last_error` a
/// consumer composes is stable across bring-ups; **§2.4 wording contract** —
/// the `last_error` for this failure is built from this list and no other
/// enumeration, e.g.
/// `format!("missing required configuration: {}", missing.join(", "))`.
///
/// Empty vec for a plugin with no `config_schema`, and for a schema with no
/// `required`: nothing is demanded, so nothing is missing.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::Manifest;
    use serde_json::json;

    /// Drive the real parser, not a hand-built `Manifest` — the schema has to
    /// survive `Manifest::validate` for these merges to mean anything.
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

    /// The read half of the PATCH `null` semantics: once the key is gone from
    /// `user_config` the default is in force again — which is what makes
    /// "clear this field" a meaningful operation instead of a way to reach an
    /// unrepresentable empty state.
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

    /// Same fixture route, minus the `config_schema` key — a plugin that
    /// declares no configurable surface has an empty effective config no
    /// matter what the row holds.
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

    /// A schema may legally omit `properties` entirely (the subset validator
    /// accepts `{type, additionalProperties}`) — it then declares no keys, so
    /// the merge is empty rather than a pass-through of whatever is stored.
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

    /// The hand-edited-row branch: `null` is not a value any `type` in this
    /// subset accepts, so a stored `null` reads as "no value" and the
    /// manifest's default is in force. (The write path never stores one — it
    /// deletes on `null` — which is exactly why this branch needs its own
    /// witness rather than riding on a route test.)
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

    /// P1-E — the consumption-side seam. A `required` key with a `default` is
    /// satisfied without the operator touching it (which is why this asks the
    /// *effective* map); one without a default and unset is what will stop a
    /// bring-up.
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

    /// The two "demands nothing" shapes, so the function is not a constant
    /// `Err`-by-another-name for plugins that never opted in.
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

    /// `user_config` is a `Value`, and a row that somehow holds a non-object
    /// must not be read as "these are your settings". The read side degrades
    /// to defaults; the *write* side refuses outright (see
    /// `patch_config_refuses_to_overwrite_a_non_object_user_config`), which is
    /// the half that could lose data.
    #[test]
    fn a_non_object_user_config_reads_as_defaults_only() {
        let m = manifest_with(schema(), 2);
        assert_eq!(
            effective_config(&m, &json!("not an object")),
            effective_config(&m, &json!({}))
        );
    }
}
