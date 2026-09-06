//! Explicit execution selection inside the already-frozen task context.
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        if kind != "codex" || spawn != "in-wave" || dependencies || gate {
            return Err("neige_execution: isolated Codex requires codex, in-wave, no dependencies and no gate".into());
        }
        Ok(())
    }
}
