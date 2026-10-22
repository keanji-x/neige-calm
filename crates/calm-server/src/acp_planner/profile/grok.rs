//! Grok Build's ACP agent profile (1.0.62). It serves ACP as `grok agent --no-leader stdio`: its
//! agent options go after `agent`, and `--no-leader` starts this process's own agent instead of
//! joining a shared leader. Its `initialize` reports no `agentInfo`, only `_meta.agentVersion`, and
//! every launch authenticates with `cached_token`, the sign-in `grok login` stored under the
//! registration's `GROK_HOME`. Its permission flags (`--allow`, `--deny`, `--permission-mode`)
//! are the TUI's and headless mode's: a stdio agent does not apply them. What a stdio agent does
//! apply is `--always-approve`, an agent option, so each turn's process runs under that turn's mode.
//!
//! `never` adds nothing: Grok asks, every request but one for Neige's own tools is answered
//! `cancelled`, and Grok then ends its turn, which says why ([`AcpAgentProfile::decline_ends_turn`]).
use super::{AcpAgentProfile, AgentEntry, AgentIdentity, ModeLaunch, NOTHING};
use provider::acp::approvals::McpToolRequest;

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    canonical_planner_inputs: false,
    canonical_assistant_inputs: false,
    entry: AgentEntry {
        before_options: &["agent"],
        after_options: &["--no-leader", "stdio"],
    },
    identity: AgentIdentity::MetaAgentVersion,
    authentication: Some("cached_token"),
    // Grok's default mode asks before edits, shell commands that write and MCP tool calls; a shell
    // command its own classifier reads as read-only runs without asking, and with no sandbox.
    ask: NOTHING,
    // No sandbox and no asks: the account's own access.
    full: ModeLaunch {
        env: &[],
        agent_options: &["--always-approve"],
    },
    // Before an MCP tool, Grok asks with `use_tool`: kind `other`, titled with the tool's name
    // qualified `<server>__<tool>`, which its `rawInput.tool_name` repeats.
    mcp_tool_request: Some(McpToolRequest {
        kind: "other",
        separator: "__",
        raw_input_name: "tool_name",
    }),
    // Any answer but an allow, `reject_once` included, ends Grok's turn `cancelled`.
    decline_ends_turn: true,
    // An xAI API key would bill the API instead of the signed-in subscription (Grok reads
    // `GROK_CODE_XAI_API_KEY` as a former name of `XAI_API_KEY`).
    refused_env: &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_permission_mode::PlannerPermissionMode;

    /// `agent <options> --no-leader stdio`: `full` adds `--always-approve`, `never` and `ask` add
    /// nothing, and no mode sets the environment.
    #[test]
    fn grok_serves_acp_as_its_stdio_agent_under_each_modes_launch() {
        let stdio = vec!["agent", "--no-leader", "stdio"];
        assert_eq!(PROFILE.argv(None).collect::<Vec<_>>(), stdio);
        for (mode, argv) in [
            (PlannerPermissionMode::Never, stdio.clone()),
            (PlannerPermissionMode::Ask, stdio.clone()),
            (
                PlannerPermissionMode::Full,
                vec!["agent", "--always-approve", "--no-leader", "stdio"],
            ),
        ] {
            assert_eq!(
                PROFILE.argv(Some(PROFILE.launch(mode))).collect::<Vec<_>>(),
                argv,
                "{mode:?}"
            );
            assert!(PROFILE.launch(mode).env.is_empty(), "{mode:?}");
        }
    }
}
