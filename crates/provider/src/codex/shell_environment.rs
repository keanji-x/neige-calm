//! Codex's configuration-layer rules for a complete explicit command environment.
use super::{
    ShellEnvironmentNames,
    error::{Error, Result},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// Build filter overrides from literal variable names without receiving their values.
///
/// The caller supplies the complete `set` map and owns its values and inheritance policy.
/// These legacy fields replace lower keyed filters in the SDK, preserving original
/// Unicode literal names instead of rewriting their table keys during layer merging.
/// Returns `Refused` for nonliteral names or inherited aliases that prevent isolation.
/// See <https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/config/src/merge.rs>.
pub fn shell_environment_filter_overrides(
    requested_names: &[String],
    inherited: Option<&ShellEnvironmentNames>,
) -> Result<Map<String, Value>> {
    if requested_names.is_empty()
        || requested_names
            .iter()
            .any(|name| name.is_empty() || name.contains(['*', '?']))
    {
        return Err(Error::Refused(
            "Codex shell environment requires nonempty literal variable names".into(),
        ));
    }
    let exact_names = requested_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let literal_names = requested_names
        .iter()
        .map(|name| literal_signature(name))
        .collect::<BTreeSet<_>>();
    if inherited.is_some_and(|policy| {
        policy.set.iter().any(|name| {
            !exact_names.contains(name.as_str()) && literal_names.contains(&literal_signature(name))
        })
    }) {
        return Err(Error::Refused(
            "Codex inherited environment has an ambiguous variable name".into(),
        ));
    }
    Ok(Map::from_iter([
        ("exclude".into(), json!([])),
        ("include_only".into(), json!(requested_names)),
    ]))
}

fn literal_signature(name: &str) -> Vec<String> {
    // The SDK compares per-character lowercase results, retaining expansion boundaries.
    name.chars()
        .map(|character| character.to_lowercase().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|name| (*name).to_owned()).collect()
    }

    fn metadata(value: Value) -> ShellEnvironmentNames {
        serde_json::from_value(value).unwrap()
    }

    fn assert_explicit_allowlist(output: &Map<String, Value>, requested: &[String]) {
        assert!(
            !output.contains_key("filters"),
            "legacy overrides must replace the inherited keyed representation"
        );
        assert_eq!(output.get("exclude"), Some(&json!([])));
        let patterns = output["include_only"].as_array().unwrap();
        assert_eq!(patterns.len(), requested.len());
        for name in requested {
            assert!(patterns.contains(&json!(name)));
        }
    }

    #[test]
    fn explicit_allowlist_replaces_inherited_canonical_filters() {
        let requested = names(&["PATH", "HTTP_PROXY", "http_proxy", "GH_TOKEN"]);
        let inherited = metadata(json!({
            "set": {"STALE_PROJECT_TOKEN": "ignored"},
            "filters": {"*": "include", "gh_token": "include"},
        }));
        let output = shell_environment_filter_overrides(&requested, Some(&inherited)).unwrap();
        assert_explicit_allowlist(&output, &requested);
    }

    #[test]
    fn inherited_set_aliases_follow_literal_matching() {
        for (requested, old, refused) in [
            ("GH_TOKEN", "gh_token", true),
            ("GH_TOKEN", "GH_TOKEN", true),
            ("ΟΣ", "οσ", true),
            ("ΟΣ", "ος", false),
            ("İ", "i\u{307}", false),
            ("i\u{307}", "İ", false),
            ("GH_ENTERPRISE_TOKEN", "GH_ENTERPRIſE_TOKEN", false),
            ("GH_TOKEN", "UNRELATED_GLOBAL", false),
            ("GH_TOKEN", "GH_TOKEN", false),
        ] {
            let inherited = metadata(json!({"set": {(old): "ignored"}}));
            let result = shell_environment_filter_overrides(&names(&[requested]), Some(&inherited));
            if refused {
                assert!(matches!(result, Err(Error::Refused(_))));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn parser_key_collisions_preserve_distinct_requested_names() {
        let inherited = metadata(json!({"filters": {}}));
        for requested in [names(&["ΟΣ", "ος"]), names(&["İ", "i\u{307}"])] {
            let output = shell_environment_filter_overrides(&requested, Some(&inherited)).unwrap();
            assert_explicit_allowlist(&output, &requested);
        }
    }

    #[test]
    fn exact_inherited_unicode_names_can_be_explicitly_replaced() {
        let inherited = metadata(json!({
            "set": {"ÄVAR": "ignored", "äVAR": "ignored"},
        }));
        assert!(
            shell_environment_filter_overrides(&names(&["ÄVAR", "äVAR"]), Some(&inherited)).is_ok()
        );
    }

    #[test]
    fn legacy_filters_keep_every_requested_literal_pattern() {
        let requested = names(&["ΟΣ", "ος", "HTTP_PROXY", "http_proxy"]);
        let output = shell_environment_filter_overrides(&requested, None).unwrap();
        assert_explicit_allowlist(&output, &requested);
    }

    #[test]
    fn empty_requested_environment_is_refused_instead_of_allowing_all() {
        for requested in [vec![], names(&[""])] {
            assert!(matches!(
                shell_environment_filter_overrides(&requested, None),
                Err(Error::Refused(_))
            ));
        }
    }

    #[test]
    fn wildcard_variable_names_are_not_interpreted_as_patterns() {
        for name in ["TOKEN*", "KEY?"] {
            assert!(matches!(
                shell_environment_filter_overrides(&names(&[name]), None),
                Err(Error::Refused(_))
            ));
        }
    }
}
