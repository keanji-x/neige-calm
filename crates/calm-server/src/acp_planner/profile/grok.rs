//! Grok Build's ACP agent profile (1.0.62). It serves ACP as `grok agent --no-leader stdio`: its
//! agent options go after `agent`, and `--no-leader` starts this process's own agent instead of
//! joining a shared leader. Its `initialize` reports no `agentInfo`, only `_meta.agentVersion`, and
//! every launch authenticates with `cached_token`, the sign-in `grok login` stored under the
//! registration's `GROK_HOME`. Its permission flags (`--allow`, `--deny`, `--permission-mode`)
//! are the TUI's and headless mode's: a stdio agent does not apply them. What a stdio agent does
//! apply is `--always-approve`, an agent option, and its sandbox profile, read from `GROK_SANDBOX`
//! when the process starts (the global `--sandbox` flag is not an agent option). A process loads a
//! saved session under its own profile, so each turn's mode holds whatever the session began under.
use super::{AcpAgentProfile, AgentEntry, AgentIdentity, ModeLaunch, NOTHING, PolicyCheck};
use provider::acp::approvals::McpToolRequest;
use serde_json::Value;

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    entry: AgentEntry {
        before_options: &["agent"],
        after_options: &["--no-leader", "stdio"],
    },
    identity: AgentIdentity::MetaAgentVersion,
    authentication: Some("cached_token"),
    // As Codex's `never`: the process runs in Grok's `workspace` sandbox (Landlock on Linux: it
    // writes only under its directory, its `GROK_HOME` and the temp directories) and never asks,
    // so an action outside the sandbox fails instead of ending the turn. `--always-approve` also
    // calls every MCP tool without asking, which is why every launch requires project MCP servers
    // off ([`PROJECT_MCP_OFF`]): only Neige's server and the operator's own are then loaded.
    never: ModeLaunch {
        env: &[("GROK_SANDBOX", "workspace")],
        agent_options: &["--always-approve"],
    },
    // Grok's default mode asks before edits, shell commands that write and MCP tool calls.
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
    policy: Some(PROJECT_MCP_OFF),
    // An xAI API key would bill the API instead of the signed-in subscription (Grok reads
    // `GROK_CODE_XAI_API_KEY` as a former name of `XAI_API_KEY`).
    refused_env: &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"],
};

/// Grok merges MCP servers a project defines (`.grok/config.toml`, `.mcp.json`, `.cursor/mcp.json`)
/// into its session, and a project server named like a client-supplied one replaces it in a folder
/// Grok does not trust. Only the policy `enable_all_project_mcp_servers = false` drops them all, and
/// `grok inspect --json` reports it enforced as `permissions.enforced[]` entry
/// `{"setting":"projectMcpServers","enabled":false}`.
const PROJECT_MCP_OFF: PolicyCheck = PolicyCheck {
    argv: &["inspect", "--json"],
    unmet: project_mcp_unmet,
};

fn project_mcp_unmet(report: &Value) -> Option<String> {
    let off = report["permissions"]["enforced"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["setting"] == "projectMcpServers" && entry["enabled"] == false);
    (!off).then(|| {
        "Grok may load MCP servers a project defines: `grok inspect --json` does not report \
         `projectMcpServers` enforced off. Add `enable_all_project_mcp_servers = false` to \
         `managed_config.toml` in the registration's `GROK_HOME`, then retry"
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_permission_mode::PlannerPermissionMode;
    use serde_json::json;

    /// `agent <options> --no-leader stdio`. `never` adds `--always-approve` inside Grok's
    /// `workspace` sandbox, `full` adds it alone, and `ask` adds nothing.
    #[test]
    fn grok_serves_acp_as_its_stdio_agent_under_each_modes_launch() {
        assert_eq!(
            PROFILE.argv(None).collect::<Vec<_>>(),
            ["agent", "--no-leader", "stdio"]
        );
        let approving = vec!["agent", "--always-approve", "--no-leader", "stdio"];
        for (mode, argv, env) in [
            (
                PlannerPermissionMode::Never,
                approving.clone(),
                &[("GROK_SANDBOX", "workspace")][..],
            ),
            (
                PlannerPermissionMode::Ask,
                vec!["agent", "--no-leader", "stdio"],
                &[],
            ),
            (PlannerPermissionMode::Full, approving, &[]),
        ] {
            assert_eq!(
                PROFILE.argv(Some(PROFILE.launch(mode))).collect::<Vec<_>>(),
                argv,
                "{mode:?}"
            );
            assert_eq!(PROFILE.launch(mode).env, env, "{mode:?}");
        }
    }

    /// Only an enforced `projectMcpServers` off, as Grok 1.0.62 reports it under the managed
    /// policy, meets the check.
    #[test]
    fn only_project_mcp_enforced_off_meets_the_policy() {
        let enforced =
            |entries: Value| json!({"grokVersion":"1.0.62","permissions":{"enforced":entries}});
        let off = json!({"setting":"projectMcpServers","enabled":false,
            "source":"/var/lib/neige/grok-home/.grok/managed_config.toml"});
        assert_eq!(project_mcp_unmet(&enforced(json!([off]))), None);
        for report in [
            json!({"grokVersion":"1.0.62","permissions":{}}),
            json!({"permissions":{"enforced":"projectMcpServers"}}),
            enforced(json!([])),
            enforced(json!([{"setting":"projectMcpServers","enabled":true}])),
            enforced(json!([{"setting":"projectMcpServers","enabled":"false"}])),
            enforced(json!([{"setting":"mcpServers","enabled":false}])),
        ] {
            let unmet = project_mcp_unmet(&report).expect("unmet");
            assert!(
                unmet.contains("enable_all_project_mcp_servers = false"),
                "{unmet}"
            );
        }
    }
}
