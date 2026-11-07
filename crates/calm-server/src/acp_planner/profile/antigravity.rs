//! Antigravity's native ACP v1 entry point. Authenticate the service account
//! with `agy acp --login` before registering it.
use super::{
    AcpAgentProfile, AgentEntry, AgentIdentity, AgentVersions, CompactionWire, ContractCompaction,
    ModeLaunch,
};

mod policy;

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    owned_options: &[
        "--gemini-dir",
        "--gemini_dir",
        "--app-data-dir",
        "--app_data_dir",
        "--cli_dir_name",
        "--settings",
        "--sandbox",
        "--mode",
        "--dangerously-skip-permissions",
        "--add-dir",
    ],
    shared_server: false,
    global_native_identity: true,
    prepare_launch: Some(policy::prepare),
    native_mode: Some("accept-edits"),
    usage: provider::acp::usage::UsageSource::SessionUsage,
    compaction: ContractCompaction {
        planner: Some(CompactionWire::CommandPrompt),
        assistant: Some(CompactionWire::CommandPrompt),
    },
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &["acp"],
        after_options: &[],
    },
    identity: AgentIdentity::AgentInfo {
        name: "Antigravity ACP Server",
    },
    versions: Some(AgentVersions {
        floor: &semver::Version::new(1, 5, 1),
        refusal: "Antigravity requires AGY 1.5.1 or later in the 1.x release line",
    }),
    owned_env: &[],
    never: policy::SANDBOXED,
    ask: policy::SANDBOXED,
    // Full still answers only this turn's requests with AllowOnce.
    full_requests: provider::acp::approvals::FullPermissionRequests::AllowOnce,
    full: policy::UNRESTRICTED,
    authentication: None,
    mcp_tool_request: None,
    decline_ends_turn: true,
    refused_env: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_permission_mode::PlannerPermissionMode;

    #[test]
    fn antigravity_rejects_the_native_sandbox_version_with_known_credential_exposure() {
        for (version, admitted) in [("1.5.0", false), ("1.5.1", true), ("1.5.3", true)] {
            let response = serde_json::from_value(serde_json::json!({"protocolVersion":1,
                "agentCapabilities":{}, "agentInfo":{"name":"Antigravity ACP Server","version":version}})).unwrap();
            assert_eq!(
                PROFILE
                    .admit_identity(&response, &semver::Version::new(1, 0, 0))
                    .is_ok(),
                admitted
            );
        }
    }

    #[test]
    fn antigravity_launch_is_managed_and_sandbox_tracks_permission() {
        assert!(
            !PROFILE.shared_server,
            "each turn must own its execution policy"
        );
        for (mode, sandbox) in [
            (PlannerPermissionMode::Never, "--sandbox=true"),
            (PlannerPermissionMode::Ask, "--sandbox=true"),
            (PlannerPermissionMode::Full, "--sandbox=false"),
        ] {
            let argv = PROFILE.argv(Some(PROFILE.launch(mode))).collect::<Vec<_>>();
            assert_eq!(argv.first(), Some(&"acp"));
            assert!(argv.contains(&sandbox), "{mode:?}: {argv:?}");
        }
    }
}
