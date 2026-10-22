//! OpenCode's ACP agent profile. It serves ACP as `opencode acp`, reports ACP's standard `agentInfo` and authenticates nothing:
//! the operator signs it in outside Neige. Its permission policy is OpenCode's own `permission`
//! configuration, overridden through the environment, since ACP has no method for it.
use super::{AcpAgentProfile, AgentEntry, AgentIdentity, ModeLaunch};

/// What OpenCode's own `permission` configuration is overridden with under `ask` (#2348): the
/// tools that act without asking under its defaults ask instead. OpenCode 1.18.35 merges this
/// variable over every config file's top-level `permission`, and has no flag or ACP method for it.
/// `edit` covers its write and patch tools too. An agent-level `permission` in the operator profile
/// (`agent.build.permission.edit = "allow"`) still wins over it, so `external_directory` keeps its
/// default `ask`: it is what still asks before a write outside the workspace then (#2452). The
/// cost is a second ask for one outside command or edit.
const ASK_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"ask","edit":"ask","webfetch":"ask"}"#,
);

/// What OpenCode's `permission` configuration is overridden with under `full` (#2441): every
/// tool that acts, reads or fetches is allowed without asking, outside the workspace too. No `*`,
/// so OpenCode keeps denying the tools a Planner has no use for (`question`, `plan_*`).
const FULL_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"allow","edit":"allow","webfetch":"allow","external_directory":"allow","doom_loop":"allow","read":"allow"}"#,
);

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    canonical_planner_inputs: true,
    canonical_assistant_inputs: true,
    entry: AgentEntry {
        before_options: &[],
        after_options: &["acp"],
    },
    identity: AgentIdentity::AgentInfo { name: "OpenCode" },
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

    /// `ask` makes OpenCode ask before bash, edits and fetches; `never` adds nothing, so its launch
    /// is today's.
    #[test]
    fn only_ask_overrides_opencodes_permission_config() {
        assert_eq!(
            PROFILE.launch(PlannerPermissionMode::Ask).env,
            [(
                "OPENCODE_PERMISSION",
                r#"{"bash":"ask","edit":"ask","webfetch":"ask"}"#
            )]
        );
        assert!(PROFILE.launch(PlannerPermissionMode::Never).env.is_empty());
    }

    /// #2441: `full` allows each acting tool by name, outside the workspace too, and never with a
    /// `*` rule, which would also allow the tools OpenCode denies a Planner.
    #[test]
    fn full_allows_each_acting_tool_by_name() {
        let [(key, value)] = PROFILE.launch(PlannerPermissionMode::Full).env else {
            panic!("one override");
        };
        assert_eq!(*key, "OPENCODE_PERMISSION");
        let permission: serde_json::Value = serde_json::from_str(value).unwrap();
        assert_eq!(
            permission,
            serde_json::json!({
                "bash": "allow",
                "edit": "allow",
                "webfetch": "allow",
                "external_directory": "allow",
                "doom_loop": "allow",
                "read": "allow",
            })
        );
        assert!(permission.get("*").is_none(), "{permission}");
    }
}
