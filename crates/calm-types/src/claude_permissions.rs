//! #1704 — the Claude Code permission scope vocabulary shared by the
//! `calm.terminal.open` argument (S1) and the Track's policy column (S2).
//!
//! A scope names `edit` globs relative to a terminal cwd, `bash` command
//! prefixes and `deny` prefixes. The rules that make a scope acceptable
//! (relative globs, no floor command, caps) live in calm-server's
//! `terminal_permissions`; this module only carries the wire shape, the
//! strict argument parser and the source marker a rendered block is stamped
//! with.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

/// A declared Claude Code permission scope: the `claude_permissions` argument
/// of `calm.terminal.open` and, since S2, the value of
/// `tracks.claude_permissions_policy`.
///
/// The derive is lenient on unknown keys on purpose: it decodes stored rows
/// and stored operation payloads, which a newer binary may have written with
/// a key this one does not know, and a list route must not fail for a whole
/// area over one such row. Strictness (unknown key, wrong list type, a JSON
/// array or `null` in place of the object) lives in [`parse_scope_named`],
/// which every untrusted entry runs first.
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

/// Parse an untrusted value into a scope, enforcing exactly the advertised
/// shape with a reason under `field` (`claude_permissions` for the tool
/// argument, `claude_permissions_policy` for the Track PATCH): an object (not
/// an array, string or null) whose keys are among `edit`, `bash`, `deny`,
/// each an array of strings (`null` is a wrong type, not an absent key). The
/// serde derive on [`ClaudePermissionsScope`] is for storage and the hash view
/// only: a derive alone would also accept a JSON array through `visit_seq`
/// and read `deny: null` as absent.
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

/// #1704 S2 — which scope a terminal's rendered `permissions` block came
/// from; stamped on the card beside the block as
/// `Card.payload.claude_permissions_source` and echoed by the open.
///
/// A card that carries `claude_permissions` and NO source predates S2 and
/// reads as [`Declared`](Self::Declared); S3 (the card view) must apply that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ClaudePermissionsSource {
    /// No Track policy: the block is exactly the Planner's declaration.
    Declared,
    /// A Track policy and no declaration: the block is the policy's.
    TrackPolicy,
    /// A Track policy and a declaration within it: the block is the merge
    /// (omitted lists inherited, given lists checked, deny appended).
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

    /// The storage derive tolerates an unknown key (a row written by a newer
    /// binary must not fail a whole list); the parser does not.
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
