//! The Claude Code permission scope vocabulary shared by the `calm.terminal.open` argument and the
//! Track's policy column: wire shape, strict argument parser and source marker.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

/// A declared Claude Code permission scope: the `claude_permissions` argument of `calm.terminal.open`
/// and the value of `tracks.claude_permissions_policy`. The derive is lenient on unknown keys on
/// purpose (stored rows from a newer binary); strictness lives in [`parse_scope_named`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(default)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ClaudePermissionsScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edit: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bash: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deny: Option<Vec<String>>,
}

/// The keys a declared scope may carry, in the schema's order.
pub const SCOPE_KEYS: [&str; 3] = ["edit", "bash", "deny"];

/// Parse an untrusted value into a scope, enforcing exactly the advertised shape with a reason under
/// `field`: an object whose keys are among `edit`, `bash`, `deny`, each an array of strings.
pub fn parse_scope_named(
    field: &str,
    value: &Value,
) -> std::result::Result<ClaudePermissionsScope, String> {
    let Some(object) = value.as_object() else {
        return Err(format!("{field}: must be an object"));
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !SCOPE_KEYS.contains(&key.as_str()))
    {
        return Err(format!("{field}: unknown key '{unknown}'"));
    }
    let list = |key: &str| -> std::result::Result<Option<Vec<String>>, String> {
        let Some(value) = object.get(key) else {
            return Ok(None);
        };
        value
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().map(str::to_owned))
                    .collect::<Option<Vec<String>>>()
            })
            .map(Some)
            .ok_or_else(|| format!("{field}.{key}: must be an array of strings"))
    };
    Ok(ClaudePermissionsScope {
        edit: list("edit")?,
        bash: list("bash")?,
        deny: list("deny")?,
    })
}

/// Which scope a terminal's rendered `permissions` block came from; stamped on the card beside the
/// block as `Card.payload.claude_permissions_source`. A card with no source reads as [`Declared`](Self::Declared).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ClaudePermissionsSource {
    /// No Track policy: the block is exactly the Planner's declaration.
    Declared,
    /// A Track policy and no declaration: the block is the policy's.
    TrackPolicy,
    /// A Track policy and a declaration within it: the block is the merge.
    DeclaredWithinPolicy,
}

impl ClaudePermissionsSource {
    /// The wire spelling (`serde(rename_all = "snake_case")`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::TrackPolicy => "track_policy",
            Self::DeclaredWithinPolicy => "declared_within_policy",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_wire_spelling_matches_as_str() {
        for source in [
            ClaudePermissionsSource::Declared,
            ClaudePermissionsSource::TrackPolicy,
            ClaudePermissionsSource::DeclaredWithinPolicy,
        ] {
            let wire = serde_json::to_value(source).unwrap();
            assert_eq!(wire, json!(source.as_str()));
            assert_eq!(
                serde_json::from_value::<ClaudePermissionsSource>(wire).unwrap(),
                source
            );
        }
        assert!(serde_json::from_value::<ClaudePermissionsSource>(json!("policy")).is_err());
    }

    #[test]
    fn derive_is_lenient_and_the_parser_is_strict() {
        let stored = json!({"edit": ["**"], "protect": ["x"]});
        assert_eq!(
            serde_json::from_value::<ClaudePermissionsScope>(stored.clone()).unwrap(),
            ClaudePermissionsScope {
                edit: Some(vec!["**".into()]),
                bash: None,
                deny: None,
            }
        );
        assert_eq!(
            parse_scope_named("claude_permissions_policy", &stored).unwrap_err(),
            "claude_permissions_policy: unknown key 'protect'"
        );
        assert_eq!(
            parse_scope_named("claude_permissions", &json!(null)).unwrap_err(),
            "claude_permissions: must be an object"
        );
        assert_eq!(
            parse_scope_named("claude_permissions_policy", &json!({"bash": "git"})).unwrap_err(),
            "claude_permissions_policy.bash: must be an array of strings"
        );
    }
}
