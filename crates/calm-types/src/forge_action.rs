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
    /// Plugin-authorized predecessor identities for this exact logical request.
    /// Used only to retrieve an existing operation in the authenticated scope.
    #[serde(default)]
    pub compatible_payload_hashes: Vec<String>,
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
        compatible_payload_hashes: Vec::new(),
    }
}

#[derive(Serialize)]
struct SemanticForgePayload<'a> {
    idem_key: &'a str,
    event_spec: Option<&'a ForgeEventSpec>,
    subject: Option<&'a ForgeMergeSubject>,
    context: &'a serde_json::Map<String, Value>,
    probe: Option<&'a ProbeSpec>,
}

pub fn semantic_payload_hash(payload: &PluginForgePayload) -> Result<String, String> {
    let semantic = SemanticForgePayload {
        idem_key: &payload.idem_key,
        event_spec: payload.event_spec.as_ref(),
        subject: payload.subject.as_ref(),
        context: &payload.context,
        probe: payload.probe.as_ref(),
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
    "forge.pr.checks",
    "forge.issue.closed",
    "worktree.provisioned",
    "worktree.committed",
    "worktree.removed",
];
