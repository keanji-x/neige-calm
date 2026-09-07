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
    #[serde(rename = "file-input")]
    FileInput,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedCodexSelection {
    pub version: IsolatedCodexVersion,
    pub workspace: IsolatedWorkspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_delivery: Option<FileDelivery>,
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
        self.validate_delivery()?;
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

/// A bounded two-node protocol, separate from ordering dependencies and gates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileDelivery {
    Producer {
        slot: String,
        path: String,
        policy: JsonDocumentPolicy,
    },
    Consumer {
        producer: String,
        slot: String,
        purpose: JsonInputPurpose,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonDocumentPolicy {
    #[serde(rename = "json-document-v1")]
    V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonInputPurpose {
    #[serde(rename = "json-input")]
    JsonInput,
}
impl IsolatedCodexSelection {
    pub fn validate_delivery(&self) -> Result<(), String> {
        let invalid = || "neige_execution: invalid single-file delivery contract".to_string();
        match (&self.workspace, &self.file_delivery) {
            (IsolatedWorkspace::Empty, None) => Ok(()),
            (IsolatedWorkspace::Empty, Some(FileDelivery::Producer { slot, path, .. })) => {
                if !valid_slot(slot)
                    || path.len() > 1024
                    || path.is_empty()
                    || path.contains(['\\', ':'])
                    || path.chars().any(char::is_control)
                    || path.split('/').count() > 32
                    || path.split('/').any(|p| {
                        p.is_empty()
                            || p == "."
                            || p == ".."
                            || p == ".codex"
                            || p == "inputs"
                            || p == ".git"
                            || p == ".gitmodules"
                    })
                {
                    return Err(invalid());
                }
                Ok(())
            }
            (IsolatedWorkspace::FileInput, Some(FileDelivery::Consumer { producer, slot, .. }))
                if crate::report_blocks::tasks::key_is_valid(producer) && valid_slot(slot) =>
            {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}
fn valid_slot(slot: &str) -> bool {
    !slot.is_empty()
        && slot.len() <= 128
        && slot
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn file_delivery_roles_paths_and_routes_are_bounded() {
        let producer = json!({"version":"isolated-codex-v1","workspace":"empty","file_delivery":{"role":"producer","slot":"result","path":"result.json","policy":"json-document-v1"}});
        let consumer = json!({"version":"isolated-codex-v1","workspace":"file-input","file_delivery":{"role":"consumer","producer":"produce","slot":"result","purpose":"json-input"}});
        for value in [&producer, &consumer] {
            let selection: IsolatedCodexSelection = serde_json::from_value(value.clone()).unwrap();
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
        for path in [
            "../x",
            "/x",
            "a//x",
            ".codex/auth.json",
            ".git/config",
            "a/.gitmodules",
            "inputs/x",
            "a\\b",
            "a/./b",
            "x:",
        ] {
            let mut value = producer.clone();
            value["file_delivery"]["path"] = json!(path);
            let selection: IsolatedCodexSelection = serde_json::from_value(value).unwrap();
            assert!(selection.validate_delivery().is_err(), "{path}");
        }
        for mut value in [producer.clone(), consumer.clone()] {
            value["workspace"] = json!(if value["workspace"] == "empty" {
                "file-input"
            } else {
                "empty"
            });
            assert!(
                serde_json::from_value::<IsolatedCodexSelection>(value)
                    .unwrap()
                    .validate_delivery()
                    .is_err()
            );
        }
        let mut both = producer;
        both["file_delivery"]["producer"] = json!("other");
        assert!(serde_json::from_value::<IsolatedCodexSelection>(both).is_err());
    }
}
