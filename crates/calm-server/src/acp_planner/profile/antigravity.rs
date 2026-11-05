//! Antigravity's native ACP v1 entry point. Authenticate the service account
//! with `agy acp --login` before registering it.
use super::{
    AcpAgentProfile, AgentEntry, AgentIdentity, AgentVersions, ContractCompaction, NOTHING,
};

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    shared_server: true,
    usage: provider::acp::usage::UsageSource::SessionUsage,
    compaction: ContractCompaction {
        planner: None,
        assistant: None,
    },
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &[],
        after_options: &["acp"],
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
    full: NOTHING,
    authentication: None,
    mcp_tool_request: None,
    decline_ends_turn: false,
    refused_env: &[],
};
