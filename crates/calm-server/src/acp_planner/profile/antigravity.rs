//! Antigravity's native ACP v1 entry point. Authenticate the service account
//! with `agy acp --login` before registering it.
use super::{
    AcpAgentProfile, AgentEntry, AgentIdentity, AgentVersions, CompactionWire, ContractCompaction,
    NOTHING,
};

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    context_snapshot: true,
    shared_server: true,
    usage: provider::acp::usage::UsageSource::SessionUsage,
    compaction: ContractCompaction {
        planner: Some(CompactionWire::CommandPrompt),
        assistant: Some(CompactionWire::CommandPrompt),
    },
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &[],
        after_options: &["acp", "--sandbox"],
    },
    identity: AgentIdentity::AgentInfo {
        name: "Antigravity ACP Server",
    },
    versions: Some(AgentVersions {
        floor: &semver::Version::new(1, 5, 0),
        refusal: "Antigravity requires AGY 1.5.0 or later in the 1.x release line",
    }),
    owned_env: &[],
    never: NOTHING,
    ask: NOTHING,
    // The resident process serves different cards concurrently. Keep its policy asking, and
    // authorize only this turn's requests; a CLI skip-permissions flag would affect every card.
    full_requests: provider::acp::approvals::FullPermissionRequests::AllowOnce,
    full: NOTHING,
    authentication: None,
    mcp_tool_request: None,
    decline_ends_turn: true,
    refused_env: &[],
};
