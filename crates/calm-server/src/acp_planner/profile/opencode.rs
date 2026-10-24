//! OpenCode 2.x ACP contract, tested from 2.1.3. The operator signs in with
//! `@opencode/cli` outside Neige; ACP authentication is deliberately not invoked.
use super::{AcpAgentProfile, AgentEntry, AgentIdentity, AgentVersions, ModeLaunch};

// Native v2.1.3 config schema: packages/schema/src/config.ts and permission.ts.
// Last match wins (core/src/permission.ts:74-87); global rules precede agent rules
// (core/src/config/plugin/agent.ts:82-123). Session rules follow agent rules;
// saved allows follow configured non-denials (core/src/permission.ts:135-165).
// These launch defaults preserve operator authority: agent rules and saved allows
// can bypass asks. Native `question` is denied because Neige has no form bridge.
// https://github.com/anomalyco/opencode/blob/v2.1.3/packages/core/src/permission.ts
// https://github.com/anomalyco/opencode/blob/v2.1.3/packages/core/src/config/plugin/agent.ts
const NEVER_PERMISSION: (&str, &str) = (
    "OPENCODE_CONFIG_CONTENT",
    r#"{"permissions":[{"action":"question","resource":"*","effect":"deny"}]}"#,
);
const ASK_PERMISSION: (&str, &str) = (
    "OPENCODE_CONFIG_CONTENT",
    r#"{"permissions":[
        {"action":"shell","resource":"*","effect":"ask"},
        {"action":"edit","resource":"*","effect":"ask"},
        {"action":"webfetch","resource":"*","effect":"ask"},
        {"action":"external_directory","resource":"*","effect":"ask"},
        {"action":"question","resource":"*","effect":"deny"}
    ]}"#,
);
// Native action inventory: core/src/tool/plugin/*.ts and file-access.ts.
// `edit` owns write and patch; no obsolete bash, doom_loop or plan_* rules.
// Only this profile needs editing when a new native action needs an override.
const FULL_PERMISSION: (&str, &str) = (
    "OPENCODE_CONFIG_CONTENT",
    r#"{"permissions":[
        {"action":"shell","resource":"*","effect":"allow"},
        {"action":"edit","resource":"*","effect":"allow"},
        {"action":"webfetch","resource":"*","effect":"allow"},
        {"action":"external_directory","resource":"*","effect":"allow"},
        {"action":"read","resource":"*","effect":"allow"},
        {"action":"grep","resource":"*","effect":"allow"},
        {"action":"glob","resource":"*","effect":"allow"},
        {"action":"websearch","resource":"*","effect":"allow"},
        {"action":"subagent","resource":"*","effect":"allow"},
        {"action":"skill","resource":"*","effect":"allow"},
        {"action":"todowrite","resource":"*","effect":"allow"},
        {"action":"opencode_list_mcp_resources","resource":"*","effect":"allow"},
        {"action":"opencode_read_mcp_resource","resource":"*","effect":"allow"},
        {"action":"question","resource":"*","effect":"deny"}
    ]}"#,
);

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    compaction: Some(super::CompactionWire::StandardPrompt),
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &[],
        after_options: &["acp"],
    },
    identity: AgentIdentity::AgentInfo { name: "OpenCode" },
    versions: Some(AgentVersions {
        floor: &semver::Version::new(2, 1, 3),
        refusal: "OpenCode requires >=2.1.3,<3.0.0; install @opencode/cli (tested floor 2.1.3)",
    }),
    owned_env: &["OPENCODE_CONFIG_CONTENT"],
    never: ModeLaunch {
        env: &[NEVER_PERMISSION],
        agent_options: &[],
    },
    authentication: None,
    ask: ModeLaunch {
        env: &[ASK_PERMISSION],
        agent_options: &[],
    },
    full: ModeLaunch {
        env: &[FULL_PERMISSION],
        agent_options: &[],
    },
    mcp_tool_request: None,
    decline_ends_turn: false,
    refused_env: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_permission_mode::PlannerPermissionMode;

    /// Every OpenCode launch, under any mode or none, runs `<command> acp` after the registration's
    /// `args`: no mode adds an agent option.
    #[test]
    fn opencode_always_serves_acp_as_its_acp_subcommand() {
        assert_eq!(PROFILE.argv(None).collect::<Vec<_>>(), ["acp"]);
        for mode in [
            PlannerPermissionMode::Never,
            PlannerPermissionMode::Ask,
            PlannerPermissionMode::Full,
        ] {
            assert_eq!(
                PROFILE.argv(Some(PROFILE.launch(mode))).collect::<Vec<_>>(),
                ["acp"],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn opencode_modes_compose_native_permission_config() {
        for mode in [
            PlannerPermissionMode::Never,
            PlannerPermissionMode::Ask,
            PlannerPermissionMode::Full,
        ] {
            let [(key, value)] = PROFILE.launch(mode).env else {
                panic!("one native config");
            };
            assert_eq!(*key, "OPENCODE_CONFIG_CONTENT");
            let config: serde_json::Value = serde_json::from_str(value).unwrap();
            let rules = config["permissions"].as_array().unwrap();
            assert!(rules.iter().all(|rule| rule["resource"] == "*"));
            assert!(rules.iter().any(|rule| rule
                == &serde_json::json!({"action":"question","resource":"*","effect":"deny"})));
            assert!(
                !rules
                    .iter()
                    .any(|rule| rule["action"] == "bash" || rule["action"] == "doom_loop")
            );
            for action in ["shell", "edit", "webfetch", "external_directory"] {
                let effect = rules
                    .iter()
                    .find(|rule| rule["action"] == action)
                    .map(|rule| rule["effect"].as_str().unwrap());
                assert_eq!(
                    effect,
                    match mode {
                        PlannerPermissionMode::Never => None,
                        PlannerPermissionMode::Ask => Some("ask"),
                        PlannerPermissionMode::Full => Some("allow"),
                    },
                    "{mode:?} {action}"
                );
            }
        }
    }
    #[test]
    fn opencode_profile_fences_supported_release_line_independently_of_operator_minimum() {
        let minimum = semver::Version::new(1, 0, 0);
        for (version, admitted) in [
            ("1.18.35", false),
            ("2.0.0", false),
            ("2.1.2", false),
            ("2.1.3-beta.1", false),
            ("2.1.3", true),
            ("2.1.3+vendor", true),
            ("2.2.0-beta.1", true),
            ("2.9.0", true),
            ("3.0.0-beta.1", false),
            ("3.0.0", false),
            ("4.0.0", false),
            ("v2.1.3", false),
            ("wrong", false),
        ] {
            let response = serde_json::from_value(serde_json::json!({"protocolVersion":1,
                "agentCapabilities":{},"agentInfo":{"name":"OpenCode","version":version}}))
            .unwrap();
            let result = PROFILE.admit_identity(&response, &minimum);
            assert_eq!(result.is_ok(), admitted, "{version}: {result:?}");
            if let Err(message) = result {
                assert!(
                    message.contains("OpenCode requires >=2.1.3,<3.0.0; install @opencode/cli"),
                    "{message}"
                );
            }
        }
    }
}
