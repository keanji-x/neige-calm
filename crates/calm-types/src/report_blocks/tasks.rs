//! Pure task-block vocabulary and diagnostics shared by report projection and plan upsert.

use crate::wave_report::ReportBlock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use utoipa::ToSchema;

pub const GATE_TIMEOUT_DEFAULT_SECS: i64 = 1800;
pub const GATE_TIMEOUT_MAX_SECS: i64 = 7200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateInput {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<i64>,
    pub steps: Vec<GateStepInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateStepInput {
    pub name: String,
    pub cmd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskDeclaration {
    pub block_index: usize,
    pub block_id: String,
    pub key: String,
    pub kind: String,
    pub goal: String,
    pub acceptance: Option<String>,
    pub gate: Option<GateInput>,
    pub no_gate_reason: Option<String>,
    pub depends_on: Vec<String>,
    pub context: Value,
    pub cwd: Option<String>,
    pub priority: i64,
    pub refs: Vec<String>,
    pub declared_by: String,
    pub released_by_user: bool,
    pub tombstoned_by_user: bool,
    pub ready: bool,
    pub tombstone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

impl Diagnostic {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Task key shape: `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn key_is_valid(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

pub fn validate_gate_shape(key: &str, gate: &GateInput) -> Result<(), String> {
    if gate.steps.is_empty() {
        return Err(format!("task {key}: gate.steps must be non-empty"));
    }
    for (index, step) in gate.steps.iter().enumerate() {
        if step.name.trim().is_empty() {
            return Err(format!(
                "task {key}: gate.steps[{index}].name must be non-empty"
            ));
        }
        if step.cmd.trim().is_empty() {
            return Err(format!(
                "task {key}: gate.steps[{index}].cmd must be non-empty"
            ));
        }
        if step.name.chars().any(|c| c.is_ascii_control())
            || step.cmd.chars().any(|c| c.is_ascii_control())
        {
            return Err(format!(
                "task {key}: gate.steps[{index}] must not contain ASCII control characters"
            ));
        }
    }
    if let Some(cwd) = gate.cwd.as_deref() {
        if cwd.chars().any(|c| c.is_ascii_control()) {
            return Err(format!(
                "task {key}: gate.cwd must not contain ASCII control characters"
            ));
        }
        let trimmed = cwd.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "task {key}: gate.cwd must be non-empty when present"
            ));
        }
        if !trimmed.starts_with('/') {
            return Err(format!(
                "task {key}: gate.cwd must be an absolute path (got `{trimmed}`)"
            ));
        }
    }
    let timeout = gate.timeout_secs.unwrap_or(GATE_TIMEOUT_DEFAULT_SECS);
    if !(1..=GATE_TIMEOUT_MAX_SECS).contains(&timeout) {
        return Err(format!(
            "task {key}: gate.timeout_secs must be in 1..={GATE_TIMEOUT_MAX_SECS} (default {GATE_TIMEOUT_DEFAULT_SECS}), got {timeout}"
        ));
    }
    Ok(())
}

pub fn dup_keys(declarations: &[TaskDeclaration]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut emitted = BTreeSet::new();
    let mut duplicates = Vec::new();
    for declaration in declarations {
        if !seen.insert(declaration.key.as_str()) && emitted.insert(declaration.key.as_str()) {
            duplicates.push(declaration.key.clone());
        }
    }
    duplicates
}

/// Keys blocked by an uncleared tombstone in the current document.
pub fn tombstoned_keys(declarations: &[TaskDeclaration]) -> Vec<String> {
    declarations
        .iter()
        .filter(|declaration| declaration.tombstone)
        .map(|declaration| declaration.key.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn unknown_deps(
    declarations: &[TaskDeclaration],
    known_task_keys: &[String],
) -> Vec<(String, String)> {
    let known: BTreeSet<&str> = declarations
        .iter()
        .filter(|declaration| !declaration.tombstone)
        .map(|d| d.key.as_str())
        .chain(known_task_keys.iter().map(String::as_str))
        .collect();
    declarations
        .iter()
        .filter(|declaration| !declaration.tombstone)
        .flat_map(|declaration| {
            declaration
                .depends_on
                .iter()
                .filter(|dependency| !known.contains(dependency.as_str()))
                .map(|dependency| (declaration.key.clone(), dependency.clone()))
        })
        .collect()
}

pub fn gate_rule_violations(declarations: &[TaskDeclaration], require_gates: bool) -> Vec<String> {
    if !require_gates {
        return Vec::new();
    }
    declarations
        .iter()
        .filter(|declaration| !declaration.tombstone)
        .filter(|declaration| {
            declaration.kind != "terminal"
                && declaration.gate.is_none()
                && declaration.no_gate_reason.is_none()
        })
        .map(|declaration| declaration.key.clone())
        .collect()
}

/// DFS cycle finder. Edges to keys outside the graph are ignored.
pub fn find_cycle(graph: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    fn visit(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        color: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        color.insert(node.to_string(), GRAY);
        path.push(node.to_string());
        for dependency in graph.get(node).into_iter().flatten() {
            if !graph.contains_key(dependency) {
                continue;
            }
            match color.get(dependency).copied().unwrap_or(WHITE) {
                GRAY => {
                    let start = path.iter().position(|key| key == dependency).unwrap_or(0);
                    let mut cycle = path[start..].to_vec();
                    cycle.push(dependency.clone());
                    return Some(cycle);
                }
                WHITE => {
                    if let Some(cycle) = visit(dependency, graph, color, path) {
                        return Some(cycle);
                    }
                }
                _ => {}
            }
        }
        path.pop();
        color.insert(node.to_string(), BLACK);
        None
    }
    let mut color = HashMap::new();
    let mut path = Vec::new();
    for node in graph.keys() {
        if color.get(node).copied().unwrap_or(WHITE) == WHITE
            && let Some(cycle) = visit(node, graph, &mut color, &mut path)
        {
            return Some(cycle);
        }
    }
    None
}

fn reachable(graph: &BTreeMap<String, Vec<String>>, from: &str, to: &str) -> bool {
    let mut pending = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        for dependency in graph.get(node).into_iter().flatten() {
            if dependency == to {
                return true;
            }
            if graph.contains_key(dependency) {
                pending.push(dependency);
            }
        }
    }
    false
}

fn cyclic_components(
    graph: &BTreeMap<String, Vec<String>>,
) -> Vec<(BTreeSet<String>, Vec<String>)> {
    let mut assigned = BTreeSet::new();
    let mut components = Vec::new();
    for node in graph.keys() {
        if assigned.contains(node) {
            continue;
        }
        let members: BTreeSet<String> = graph
            .keys()
            .filter(|candidate| {
                reachable(graph, node, candidate) && reachable(graph, candidate, node)
            })
            .cloned()
            .collect();
        let cyclic = members.len() > 1
            || graph
                .get(node)
                .is_some_and(|dependencies| dependencies.contains(node));
        if !cyclic {
            continue;
        }
        assigned.extend(members.iter().cloned());
        let component_graph: BTreeMap<String, Vec<String>> = members
            .iter()
            .map(|key| {
                let dependencies = graph[key]
                    .iter()
                    .filter(|dependency| members.contains(*dependency))
                    .cloned()
                    .collect();
                (key.clone(), dependencies)
            })
            .collect();
        let cycle = find_cycle(&component_graph).expect("cyclic component has a cycle");
        components.push((members, cycle));
    }
    components
}

/// Finds a representative path for every cyclic component.
pub fn find_cycles(graph: &BTreeMap<String, Vec<String>>) -> Vec<Vec<String>> {
    cyclic_components(graph)
        .into_iter()
        .map(|(_, cycle)| cycle)
        .collect()
}

pub fn declaration_graph(declarations: &[TaskDeclaration]) -> BTreeMap<String, Vec<String>> {
    declarations
        .iter()
        .filter(|declaration| !declaration.tombstone)
        .map(|d| (d.key.clone(), d.depends_on.clone()))
        .collect()
}

/// Project task declarations and block-local/document-local diagnostics without reading the DB.
pub fn project_task_declarations(
    blocks: &[ReportBlock],
) -> (Vec<TaskDeclaration>, Vec<Vec<Diagnostic>>) {
    let mut declarations = Vec::new();
    let mut diagnostics = vec![Vec::new(); blocks.len()];
    for (index, block) in blocks.iter().enumerate() {
        if block.kind != super::KIND_TASK {
            continue;
        }
        if let Err(error) = super::validate_payload(super::KIND_TASK, &block.payload) {
            diagnostics[index].push(Diagnostic::new("payload", error));
            continue;
        }
        let payload = block
            .payload
            .as_object()
            .expect("validated task payload is an object");
        let tombstone = payload
            .get("tombstone")
            .is_some_and(|value| !value.is_null());
        let declaration = TaskDeclaration {
            block_index: index,
            block_id: block.id.clone(),
            key: payload["key"].as_str().expect("validated key").to_string(),
            kind: payload
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            goal: payload
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            acceptance: payload
                .get("acceptance")
                .and_then(Value::as_str)
                .map(str::to_string),
            gate: payload
                .get("gate")
                .map(|gate| serde_json::from_value(gate.clone()).expect("validated gate")),
            no_gate_reason: payload
                .get("no_gate_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            depends_on: payload
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            context: payload
                .get("context")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string),
            priority: payload.get("priority").and_then(Value::as_i64).unwrap_or(0),
            refs: payload
                .get("refs")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            declared_by: payload["declared_by"]
                .as_str()
                .expect("validated declared_by")
                .to_string(),
            released_by_user: payload
                .get("released_by_user")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tombstoned_by_user: payload.get("tombstoned_by").and_then(Value::as_str)
                == Some("user"),
            ready: payload
                .get("ready")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tombstone,
        };
        declarations.push(declaration);
    }
    let tombstoned: BTreeSet<String> = tombstoned_keys(&declarations).into_iter().collect();
    for duplicate in dup_keys(&declarations)
        .into_iter()
        .filter(|duplicate| !tombstoned.contains(duplicate))
    {
        for (index, _block) in blocks.iter().enumerate().filter(|(_, block)| {
            block.kind == super::KIND_TASK
                && block.payload.get("key").and_then(Value::as_str) == Some(duplicate.as_str())
        }) {
            diagnostics[index].push(Diagnostic::new(
                "key",
                format!("duplicate key `{duplicate}`"),
            ));
        }
    }
    for key in &tombstoned {
        for (index, _block) in blocks.iter().enumerate().filter(|(_, block)| {
            block.kind == super::KIND_TASK
                && block.payload.get("key").and_then(Value::as_str) == Some(key.as_str())
                && block.payload.get("tombstone").is_none_or(Value::is_null)
        }) {
            diagnostics[index].push(Diagnostic::new(
                "key",
                format!("task key `{key}` has an uncleared tombstone"),
            ));
        }
    }
    let graph = declaration_graph(&declarations);
    for (keys, cycle) in cyclic_components(&graph) {
        for (index, _block) in blocks.iter().enumerate().filter(|(_, block)| {
            block.kind == super::KIND_TASK
                && block
                    .payload
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|key| keys.contains(key))
        }) {
            diagnostics[index].push(Diagnostic::new(
                "depends_on",
                format!("dependency cycle: {}", cycle.join(" -> ")),
            ));
        }
    }
    (declarations, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration(key: &str, dependencies: &[&str]) -> TaskDeclaration {
        TaskDeclaration {
            block_index: 0,
            block_id: key.into(),
            key: key.into(),
            kind: "codex".into(),
            goal: "goal".into(),
            acceptance: None,
            gate: None,
            no_gate_reason: None,
            depends_on: dependencies.iter().map(|value| (*value).into()).collect(),
            context: serde_json::json!({}),
            cwd: None,
            priority: 0,
            refs: Vec::new(),
            declared_by: "spec".into(),
            released_by_user: false,
            tombstoned_by_user: false,
            ready: true,
            tombstone: false,
        }
    }

    fn tombstone(key: &str) -> TaskDeclaration {
        TaskDeclaration {
            tombstone: true,
            ..declaration(key, &[])
        }
    }

    #[test]
    fn pure_predicates_cover_duplicates_unknown_dependencies_cycles_and_gate_policy() {
        let declarations = vec![declaration("a", &["missing"]), declaration("a", &[])];
        assert_eq!(dup_keys(&declarations), vec!["a"]);
        assert_eq!(
            unknown_deps(&declarations, &[]),
            vec![("a".into(), "missing".into())]
        );
        assert_eq!(gate_rule_violations(&declarations, true), vec!["a", "a"]);
        let graph = declaration_graph(&[declaration("a", &["b"]), declaration("b", &["a"])]);
        assert_eq!(
            find_cycle(&graph),
            Some(vec!["a".into(), "b".into(), "a".into()])
        );
    }

    #[test]
    fn tombstones_only_participate_in_duplicate_and_tombstone_predicates() {
        let declarations = vec![declaration("consumer", &["removed"]), tombstone("removed")];
        assert!(dup_keys(&declarations).is_empty());
        assert_eq!(tombstoned_keys(&declarations), vec!["removed"]);
        assert_eq!(
            unknown_deps(&declarations, &[]),
            vec![("consumer".into(), "removed".into())]
        );
        assert!(gate_rule_violations(&declarations, true).contains(&"consumer".into()));
        assert!(!gate_rule_violations(&declarations, true).contains(&"removed".into()));
        assert!(!declaration_graph(&declarations).contains_key("removed"));

        let duplicate = vec![tombstone("removed"), declaration("removed", &[])];
        assert_eq!(dup_keys(&duplicate), vec!["removed"]);
    }

    #[test]
    fn find_cycles_returns_one_path_per_disjoint_component() {
        let graph = declaration_graph(&[
            declaration("a", &["b"]),
            declaration("b", &["a"]),
            declaration("x", &["y"]),
            declaration("y", &["x"]),
        ]);
        assert_eq!(
            find_cycles(&graph),
            vec![
                vec![String::from("a"), String::from("b"), String::from("a")],
                vec![String::from("x"), String::from("y"), String::from("x")]
            ]
        );
    }

    #[test]
    fn duplicate_keys_follow_the_order_they_first_repeat() {
        let declarations = ["z", "z", "a", "a"].map(|key| declaration(key, &[]));
        assert_eq!(dup_keys(&declarations), vec!["z", "a"]);
    }

    #[test]
    fn projection_is_pure_and_reports_document_level_diagnostics() {
        let blocks = vec![
            ReportBlock {
                id: "b_0001".into(),
                kind: super::super::KIND_TASK.into(),
                rev: 0,
                payload: json!({"key":"a","kind":"codex","goal":"g","depends_on":[],"ready":true,"declared_by":"spec"}),
            },
            ReportBlock {
                id: "b_0002".into(),
                kind: super::super::KIND_TASK.into(),
                rev: 0,
                payload: json!({"key":"a","kind":"terminal","goal":"g","ready":true,"declared_by":"spec"}),
            },
        ];
        let (declarations, diagnostics) = project_task_declarations(&blocks);
        assert_eq!(declarations.len(), 2);
        assert!(
            diagnostics
                .iter()
                .flatten()
                .any(|d| d.message.contains("duplicate key"))
        );
    }

    #[test]
    fn projection_gives_redeclaration_the_tombstone_diagnostic() {
        let blocks = vec![
            ReportBlock {
                id: "b_0001".into(),
                kind: super::super::KIND_TASK.into(),
                rev: 0,
                payload: json!({"key":"removed","tombstone":{},"declared_by":"spec","tombstoned_by":"user"}),
            },
            ReportBlock {
                id: "b_0002".into(),
                kind: super::super::KIND_TASK.into(),
                rev: 0,
                payload: json!({"key":"removed","kind":"codex","goal":"again","ready":true,"declared_by":"spec"}),
            },
        ];
        let (declarations, diagnostics) = project_task_declarations(&blocks);
        assert_eq!(dup_keys(&declarations), vec!["removed"]);
        assert!(diagnostics[0].is_empty());
        assert_eq!(diagnostics[1].len(), 1);
        assert_eq!(diagnostics[1][0].path, "key");
        assert_eq!(
            diagnostics[1][0].message,
            "task key `removed` has an uncleared tombstone"
        );
    }

    #[test]
    fn projection_reports_every_disjoint_cycle_only_on_task_blocks() {
        let task = |id: &str, key: &str, dependency: &str| ReportBlock {
            id: id.into(),
            kind: super::super::KIND_TASK.into(),
            rev: 0,
            payload: json!({"key":key,"kind":"codex","goal":"g","depends_on":[dependency],"ready":true,"declared_by":"spec"}),
        };
        let mut blocks = vec![
            task("b_0001", "a", "b"),
            task("b_0002", "b", "a"),
            task("b_0003", "x", "y"),
            task("b_0004", "y", "x"),
        ];
        blocks.push(ReportBlock {
            id: "b_0005".into(),
            kind: "prose".into(),
            rev: 0,
            payload: json!({"key":"a","markdown":"not a task"}),
        });
        let (_, diagnostics) = project_task_declarations(&blocks);
        for block_diagnostics in diagnostics.iter().take(4) {
            assert!(
                block_diagnostics
                    .iter()
                    .any(|d| d.message.contains("dependency cycle"))
            );
        }
        assert!(diagnostics[4].is_empty());
    }
}
