//! Forge action vocabulary and semantic identity shared by plugins and the kernel.
use crate::event::{ForgeEventSpec, ForgeMergeSubject};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProbeSpec {
    pub probe_argv: Vec<String>,
    #[serde(default)]
    pub output_probe_argv: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PluginForgePayload {
    pub argv: Vec<String>,
    pub idem_key: String,
    #[serde(default)]
    pub event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    pub subject: Option<ForgeMergeSubject>,
    #[serde(default)]
    pub context: serde_json::Map<String, Value>,
    #[serde(default)]
    pub probe: Option<ProbeSpec>,
    #[serde(default)]
    pub parked: bool,
    /// The plugin declares this call an idempotent read whose `idem_key` encodes every semantic
    /// argument: an operation already holding the key is replayed whatever program version
    /// submitted it. Not part of the semantic hash.
    #[serde(default)]
    pub replay_any_version: bool,
    /// The plugin gates this call on the user's recorded approval: the kernel runs it only after
    /// the user clicked Approve on the ask it raises for this exact idempotency key and payload.
    /// Part of the semantic hash when present.
    #[serde(default)]
    pub approval: Option<ForgeApprovalRequest>,
}

/// What the user is asked to approve, rendered by the plugin; the kernel supplies the choices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeApprovalRequest {
    pub title: String,
}

/// Construct a forge action with event extraction and recovery probes.
pub fn forge_action_payload(
    idem_key: String,
    argv: Vec<String>,
    table: ForgeEventSpec,
    probes: ProbeSpec,
) -> PluginForgePayload {
    PluginForgePayload {
        argv,
        idem_key,
        event_spec: Some(table),
        subject: None,
        context: Map::new(),
        probe: Some(probes),
        parked: false,
        replay_any_version: false,
        approval: None,
    }
}

#[derive(Serialize)]
struct SemanticForgePayload<'a> {
    idem_key: &'a str,
    event_spec: Option<&'a ForgeEventSpec>,
    subject: Option<&'a ForgeMergeSubject>,
    context: &'a serde_json::Map<String, Value>,
    probe: Option<&'a ProbeSpec>,
    /// Absent unless set, so every hash of an ungated call is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<&'a ForgeApprovalRequest>,
}

pub fn semantic_payload_hash(payload: &PluginForgePayload) -> Result<String, String> {
    let semantic = SemanticForgePayload {
        idem_key: &payload.idem_key,
        event_spec: payload.event_spec.as_ref(),
        subject: payload.subject.as_ref(),
        context: &payload.context,
        probe: payload.probe.as_ref(),
        approval: payload.approval.as_ref(),
    };
    let bytes = serde_json::to_vec(&semantic)
        .map_err(|e| format!("forge-action hash serialization: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

pub const SUPPORTED_FORGE_EVENT_KINDS: &[&str] = &[
    "forge.pr.merged",
    "forge.scan.completed",
    "forge.pr.opened",
    "forge.pr.published",
    "forge.pr.diff.read",
    "forge.issue.read",
    "forge.issue.created",
    "forge.issue.searched",
    "forge.pr.checks",
    "forge.ci.evidence.read",
    "forge.issue.closed",
    "worktree.provisioned",
    "worktree.committed",
    "worktree.removed",
];

/// Forge results whose lifecycle continues in the Planner. Receipt-only calls
/// return directly to their caller and do not create another Planner turn.
pub const PLANNER_RESULT_KINDS: &[&str] = &[
    "forge.scan.completed",
    "forge.pr.opened",
    "forge.pr.checks",
    "forge.issue.closed",
    "forge.pr.merged",
];

pub fn notifies_planner(kind: &str) -> bool {
    PLANNER_RESULT_KINDS.contains(&kind)
}
