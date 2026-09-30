//! Explicit execution selection inside the already-frozen task context.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// #1830 S2 D5: whether a task runs in its track's checkout — a codex or claude task that is
/// neither isolated (a `neige_execution` selection in its context) nor on the child-track route.
/// Such tasks share the checkout, so they run one at a time.
pub fn runs_in_track_checkout(kind: &str, spawn: &str, context: &Value) -> bool {
    matches!(kind, "codex" | "claude")
        && spawn != crate::task_recovery::TASK_CHILD_TRACK_ROUTE
        && context.get("neige_execution").is_none()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsolatedCodexVersion {
    #[serde(rename = "isolated-codex-v1")]
    V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsolatedWorkspace {
    #[serde(rename = "empty")]
    Empty,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedCodexSelection {
    pub version: IsolatedCodexVersion,
    pub workspace: IsolatedWorkspace,
    /// Exact platform-proxied plugin grants. Historical tasks delegate none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_tools: Vec<String>,
}
impl IsolatedCodexSelection {
    /// Missing reserved field is legacy. A present invalid field is never legacy.
    pub fn from_context(context: &Value) -> Result<Option<Self>, String> {
        context
            .get("neige_execution")
            .map(|selection| {
                serde_json::from_value(selection.clone()).map_err(|error| {
                    format!("neige_execution: unsupported isolated Codex selection: {error}")
                })
            })
            .transpose()
    }
    pub fn validate_route(
        &self,
        kind: &str,
        spawn: &str,
        dependencies: bool,
        gate: bool,
    ) -> Result<(), String> {
        validate_plugin_tools(&self.plugin_tools)?;
        if kind != "codex"
            || spawn != crate::task_recovery::TASK_IN_TRACK_ROUTE
            || dependencies
            || gate
        {
            return Err("neige_execution: isolated Codex requires codex within the parent Track, no dependencies and no gate".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn isolated_routes_are_bounded() {
        let empty = json!({"version":"isolated-codex-v1","workspace":"empty"});
        let selection: IsolatedCodexSelection = serde_json::from_value(empty).unwrap();
        selection
            .validate_route(
                "codex",
                crate::task_recovery::TASK_IN_TRACK_ROUTE,
                false,
                false,
            )
            .unwrap();
        for (kind, spawn, deps, gate) in [
            (
                "claude",
                crate::task_recovery::TASK_IN_TRACK_ROUTE,
                false,
                false,
            ),
            (
                "codex",
                crate::task_recovery::TASK_CHILD_TRACK_ROUTE,
                false,
                false,
            ),
            (
                "codex",
                crate::task_recovery::TASK_IN_TRACK_ROUTE,
                true,
                false,
            ),
            (
                "codex",
                crate::task_recovery::TASK_IN_TRACK_ROUTE,
                false,
                true,
            ),
        ] {
            assert!(selection.validate_route(kind, spawn, deps, gate).is_err());
        }
    }
}

/// Validate an explicit bounded grant set; annotations and wildcards confer no authority.
pub fn validate_plugin_tools(names: &[String]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    if names.len() > 32 {
        return Err("plugin_tools: at most 32 exact plugin tool names are allowed".into());
    }
    for name in names {
        let valid = name
            .strip_prefix("plugin.")
            .and_then(|rest| rest.split_once('_'))
            .is_some_and(|(plugin, tool)| !plugin.is_empty() && !tool.is_empty());
        if !valid
            || name.len() > 256
            || name
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '*')
            || !seen.insert(name)
        {
            return Err("plugin_tools: expected unique exact plugin.<id>_<tool> names".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod plugin_grant_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn isolated_plugin_grants_preserve_legacy_shape_and_reject_ambiguous_names() {
        let legacy = json!({"version":"isolated-codex-v1","workspace":"empty"});
        let selection: IsolatedCodexSelection = serde_json::from_value(legacy.clone()).unwrap();
        assert!(selection.plugin_tools.is_empty());
        assert_eq!(serde_json::to_value(selection).unwrap(), legacy);
        for names in [
            vec!["*"],
            vec!["calm.report.commit"],
            vec!["plugin.foo_*"],
            vec!["plugin.foo_read", "plugin.foo_read"],
            vec!["plugin.foo_read\n"],
            vec!["plugin._read"],
        ] {
            assert!(
                validate_plugin_tools(&names.into_iter().map(String::from).collect::<Vec<_>>())
                    .is_err()
            );
        }
        let granted:IsolatedCodexSelection=serde_json::from_value(json!({"version":"isolated-codex-v1","workspace":"empty","plugin_tools":["plugin.research_lookup"]})).unwrap();
        validate_plugin_tools(&granted.plugin_tools).unwrap();
    }
}
