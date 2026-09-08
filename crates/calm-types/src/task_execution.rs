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
    CandidateProducer {
        slot: String,
        paths: Vec<String>,
        policy: CandidateMachinePolicy,
    },
    CandidateConsumer {
        producer: String,
        slot: String,
        purpose: CandidateInputPurpose,
    },
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
#[serde(deny_unknown_fields)]
pub struct CandidateMachinePolicy {
    pub scope: CandidateCheckScope,
    pub timeout_secs: u32,
    pub steps: Vec<CandidateCheck>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateCheck {
    pub name: String,
    pub cmd: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateCheckScope {
    #[serde(rename = "declared-checks-only")]
    DeclaredChecksOnly,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateInputPurpose {
    #[serde(rename = "verified-candidate-input")]
    VerifiedCandidateInput,
}
impl CandidateMachinePolicy {
    pub fn validate(&self) -> Result<(), String> {
        const PATH: &str = "neige_execution.file_delivery.policy";
        if !(1..=7200).contains(&self.timeout_secs) {
            return Err(format!("{PATH}.timeout_secs must be between 1 and 7200"));
        }
        if self.steps.is_empty() || self.steps.len() > 32 {
            return Err(format!("{PATH}.steps must contain between 1 and 32 checks"));
        }
        let mut names = std::collections::BTreeMap::new();
        for (index, step) in self.steps.iter().enumerate() {
            let path = format!("{PATH}.steps[{index}]");
            if step.name.trim().is_empty() {
                return Err(format!("{path}.name must be non-empty"));
            }
            if step.name.len() > 128 {
                return Err(format!("{path}.name must be at most 128 UTF-8 bytes"));
            }
            if step.name.chars().any(char::is_control) {
                return Err(format!("{path}.name must not contain control characters"));
            }
            if step.cmd.trim().is_empty() {
                return Err(format!("{path}.cmd must be non-empty"));
            }
            if step.cmd.len() > 16384 {
                return Err(format!("{path}.cmd must be at most 16384 UTF-8 bytes"));
            }
            if step.cmd.chars().any(|c| c.is_ascii_control()) {
                return Err(format!(
                    "{path}.cmd must not contain ASCII control characters"
                ));
            }
            // The shared gate log reader trims sentinel labels. Reject names
            // that would become ambiguous there, without rewriting either name
            // or command bytes in the frozen policy.
            if let Some(previous) = names.insert(step.name.trim(), index) {
                return Err(format!(
                    "{path}.name duplicates {PATH}.steps[{previous}].name"
                ));
            }
        }
        Ok(())
    }
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
            (
                IsolatedWorkspace::Empty,
                Some(FileDelivery::CandidateProducer {
                    slot,
                    paths,
                    policy,
                }),
            ) => {
                if paths.is_empty() || paths.len() > 64 {
                    return Err(invalid());
                }
                policy.validate()?;
                if !valid_slot(slot) {
                    return Err(invalid());
                }
                let mut seen = std::collections::BTreeSet::new();
                let mut entries = std::collections::BTreeSet::new();
                for path in paths {
                    if !valid_delivery_path(path) || !seen.insert(path) {
                        return Err(invalid());
                    }
                    entries.insert(path.as_str());
                    let mut parent = path.as_str();
                    while let Some((directory, _)) = parent.rsplit_once('/') {
                        entries.insert(directory);
                        parent = directory;
                    }
                }
                // Materialization adds the structural `source` directory to this set.
                if entries.len() + 1 > 64 {
                    return Err(invalid());
                }
                // Manifest repeats declared paths in outputs and entries. Reserve
                // bounded metadata (digest, mode, byte length) before launching work.
                let encoded_paths = paths
                    .iter()
                    .map(|p| serde_json::to_string(p).map(|s| s.len()))
                    .chain(
                        entries
                            .iter()
                            .map(|p| serde_json::to_string(p).map(|s| s.len())),
                    )
                    .try_fold(0usize, |total, length| length.map(|length| total + length))
                    .map_err(|_| invalid())?;
                if encoded_paths + entries.len() * 256 + 512 > 64 * 1024 {
                    return Err(invalid());
                }
                if paths.iter().any(|a| {
                    paths
                        .iter()
                        .any(|b| a != b && b.starts_with(&format!("{a}/")))
                }) {
                    return Err(invalid());
                }
                Ok(())
            }
            (
                IsolatedWorkspace::FileInput,
                Some(FileDelivery::CandidateConsumer { producer, slot, .. }),
            ) if crate::report_blocks::tasks::key_is_valid(producer) && valid_slot(slot) => Ok(()),
            (IsolatedWorkspace::Empty, Some(FileDelivery::Producer { slot, path, .. })) => {
                if !valid_slot(slot) || !valid_delivery_path(path) {
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
fn valid_delivery_path(path: &str) -> bool {
    !(path.len() > 1024
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
        }))
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
    fn candidate_policy_human_names_and_indexed_errors() {
        let base = json!({"scope":"declared-checks-only","timeout_secs":60,"steps":[
            {"name":"first","cmd":"true"},{"name":"second","cmd":"true"}
        ]});
        for name in [
            "六个独立测试全部通过".to_owned(),
            "直接调用规则验证".into(),
            "中文 user's \"quoted\" $HOME $(true)".into(),
            "é".repeat(64),
        ] {
            let mut value = base.clone();
            value["steps"][0]["name"] = json!(name);
            serde_json::from_value::<CandidateMachinePolicy>(value)
                .unwrap()
                .validate()
                .unwrap();
        }
        let cases = [
            ("/timeout_secs", json!(0), "policy.timeout_secs"),
            ("/timeout_secs", json!(7201), "policy.timeout_secs"),
            ("/steps", json!([]), "policy.steps"),
            (
                "/steps",
                json!(
                    (0..33)
                        .map(|i| json!({"name":format!("step-{i}"),"cmd":"true"}))
                        .collect::<Vec<_>>()
                ),
                "policy.steps",
            ),
            ("/steps/1/name", json!(""), "policy.steps[1].name"),
            ("/steps/1/name", json!(" \u{2003} "), "policy.steps[1].name"),
            (
                "/steps/1/name",
                json!("é".repeat(65)),
                "policy.steps[1].name",
            ),
            ("/steps/1/name", json!("bad\0name"), "policy.steps[1].name"),
            ("/steps/1/name", json!("bad\nname"), "policy.steps[1].name"),
            (
                "/steps/1/name",
                json!("bad\u{0085}name"),
                "policy.steps[1].name",
            ),
            ("/steps/1/cmd", json!(" "), "policy.steps[1].cmd"),
            ("/steps/1/cmd", json!("echo\0bad"), "policy.steps[1].cmd"),
            ("/steps/1/cmd", json!("echo\nbad"), "policy.steps[1].cmd"),
            (
                "/steps/1/cmd",
                json!("x".repeat(16385)),
                "policy.steps[1].cmd",
            ),
            ("/steps/1/name", json!("first"), "policy.steps[1].name"),
            ("/steps/1/name", json!(" first "), "policy.steps[1].name"),
        ];
        for (pointer, replacement, expected_path) in cases {
            let mut value = base.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            let error = serde_json::from_value::<CandidateMachinePolicy>(value)
                .unwrap()
                .validate()
                .unwrap_err();
            assert!(error.contains(expected_path), "{pointer}: {error}");
            if pointer == "/steps/1/name" && error.contains("duplicates") {
                assert!(error.contains("policy.steps[0].name"), "{error}");
            }
        }
    }

    #[test]
    fn candidate_contract_bounds_structural_entries_and_requires_machine_policy() {
        let value = json!({"version":"isolated-codex-v1","workspace":"empty","file_delivery":{"role":"candidate_producer","slot":"project","paths":["src/main.py","README.md","tests/test_main.py"],"policy":{"scope":"declared-checks-only","timeout_secs":20,"steps":[{"name":"test","cmd":"python3 -m unittest"}]}}});
        let selected: IsolatedCodexSelection = serde_json::from_value(value.clone()).unwrap();
        selected.validate_delivery().unwrap();
        for paths in [
            (0..64).map(|i| format!("src/{i}.py")).collect::<Vec<_>>(),
            vec!["a".into(), "a/b".into()],
            vec!["a".into(), "a".into()],
        ] {
            let mut invalid = value.clone();
            invalid["file_delivery"]["paths"] = json!(paths);
            assert!(
                serde_json::from_value::<IsolatedCodexSelection>(invalid)
                    .unwrap()
                    .validate_delivery()
                    .is_err()
            );
        }
        let mut oversized_manifest = value.clone();
        let prefix = format!(
            "{}/{}/{}",
            "a".repeat(200),
            "b".repeat(200),
            "c".repeat(200)
        );
        oversized_manifest["file_delivery"]["paths"] = json!(
            (0..50)
                .map(|i| format!("{prefix}/{}-{i}", "d".repeat(200)))
                .collect::<Vec<_>>()
        );
        assert!(
            serde_json::from_value::<IsolatedCodexSelection>(oversized_manifest)
                .unwrap()
                .validate_delivery()
                .is_err()
        );
        for (field, replacement) in [("scope", json!("review-required")), ("cwd", json!("/tmp"))] {
            let mut invalid = value.clone();
            invalid["file_delivery"]["policy"][field] = replacement;
            assert!(serde_json::from_value::<IsolatedCodexSelection>(invalid).is_err());
        }
        for field in ["scope", "timeout_secs", "steps"] {
            let mut invalid = value.clone();
            invalid["file_delivery"]["policy"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<IsolatedCodexSelection>(invalid).is_err());
        }
    }
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
