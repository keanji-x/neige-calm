//! Google's standalone antigravity-acp service; authentication belongs to its native settings.
use super::{
    AcpAgentProfile, AgentEntry, AgentIdentity, AgentVersions, ContractCompaction, NOTHING,
};

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    native_session_contract: Some("antigravity-acp-v1"),
    usage: provider::acp::usage::UsageSource::SessionUsage,
    compaction: ContractCompaction {
        planner: None,
        assistant: None,
    },
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &[],
        after_options: &[],
    },
    identity: AgentIdentity::AgentInfo {
        name: "antigravity-acp",
    },
    versions: Some(AgentVersions {
        floor: &semver::Version::new(1, 4, 5),
        refusal: "Use standalone antigravity-acp >=1.4.5,<2.0.0; create a new conversation for CLI history",
    }),
    owned_env: &[],
    never: NOTHING,
    ask: NOTHING,
    // Approve only requests owned by this turn; never bypass the service's native policy.
    full_requests: provider::acp::approvals::FullPermissionRequests::AllowOnce,
    full: NOTHING,
    authentication: None,
    mcp_tool_request: None,
    decline_ends_turn: true,
    refused_env: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standalone_antigravity_uses_the_official_identity_and_process_contract() {
        let response = serde_json::from_value(json!({"protocolVersion":1,
            "agentCapabilities":{"loadSession":true,"sessionCapabilities":{"resume":{}}},
            "agentInfo":{"name":"antigravity-acp","version":"1.4.5"}}))
        .unwrap();
        assert!(
            PROFILE
                .admit_identity(&response, &semver::Version::new(1, 4, 5))
                .is_ok()
        );
        assert_eq!(PROFILE.argv(None).collect::<Vec<_>>(), Vec::<&str>::new());
        assert!(!PROFILE.manual_compaction());
    }

    #[test]
    fn standalone_antigravity_refuses_the_cli_and_unsupported_release_lines() {
        for (name, version) in [
            ("Antigravity ACP Server", "1.5.3"),
            ("antigravity-acp", "1.4.4"),
            ("antigravity-acp", "2.0.0"),
        ] {
            let response = serde_json::from_value(json!({"protocolVersion":1,
                "agentCapabilities":{},"agentInfo":{"name":name,"version":version}}))
            .unwrap();
            assert!(
                PROFILE
                    .admit_identity(&response, &semver::Version::new(1, 0, 0))
                    .is_err()
            );
        }
    }
}
