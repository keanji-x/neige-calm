use super::{Error, Result};
use serde_json::{Value, json};
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub const DELIVERY_PROFILE: &str = "neige-delivery-v1";
pub const WORKSPACE: &str = "/workspace";
pub const PROVIDER_HOME: &str = "/provider/home";
pub const PROVIDER_CONTROL: &str = "/provider/control";
pub const PROVIDER_MCP: &str = "/provider/mcp";
pub const PROVIDER_BIN: &str = "/provider-bin";
/// Shell PATH inside the executor. Only `/provider-bin` is populated by the
/// kernel; `/usr/bin` and `/bin` are read-only host mounts whose contents the
/// kernel never enumerates.
pub const EXECUTOR_PATH: &str = "/provider-bin:/usr/bin:/bin";
/// The only writable roots inside the executor.
pub const WRITABLE_ROOTS: [&str; 2] = [WORKSPACE, "/tmp"];
/// Outbound network for the model's own commands. Native MCP reaches the kernel
/// over a private unix socket and is not affected by this switch.
pub const NETWORK_ENABLED: bool = false;
/// Provider web search. `disabled` also removes the search tool from the model.
pub const WEB_SEARCH: &str = "disabled";
/// Optional provider routes that could introduce another execution or external
/// tool. Every entry is forced off on every attempt.
pub const DISABLED_FEATURES: [&str; 15] = [
    "apps",
    "enable_mcp_apps",
    "multi_agent",
    "multi_agent_v2",
    "code_mode",
    "code_mode_only",
    "js_repl",
    "js_repl_tools_only",
    "remote_control",
    "remote_plugin",
    "standalone_web_search",
    "web_search",
    "web_search_request",
    "request_permissions",
    "request_permissions_tool",
];
/// Native MCP tools the executor may call. Card-role admission still applies.
pub const MCP_TOOL_ALLOWLIST: [&str; 4] = [
    "calm.task.complete",
    "calm.task.fail",
    "calm.report.read",
    "calm.plan.list",
];

/// The executor environment as a Planner-facing statement. Every value comes
/// from the constants `apply` writes into the provider policy document and the
/// executable mounts `bootstrap` injects, so the statement cannot drift from
/// the enforced envelope. The same envelope applies to every attempt of a
/// task, including recoveries: recovery only provides a new workspace.
pub fn executor_environment() -> Value {
    executor_environment_with_plugins(&[])
}

pub fn executor_environment_with_plugins(plugin_tools: &[String]) -> Value {
    let tools: Vec<&str> = MCP_TOOL_ALLOWLIST
        .iter()
        .copied()
        .chain(plugin_tools.iter().map(String::as_str))
        .collect();
    json!({
        "executor": "codex",
        "workspace": {
            "root": WORKSPACE,
            "writable": WRITABLE_ROOTS,
            "fresh_per_attempt": true,
        },
        "network": {
            "enabled": NETWORK_ENABLED,
            "web_search": WEB_SEARCH != "disabled",
        },
        "path": EXECUTOR_PATH,
        "provided_binaries": super::bootstrap::PROVIDER_BIN_NAMES,
        "host_usr": "read-only mount; contents not enumerated by the kernel",
        "mcp_tools": tools,
        "plugin_tools": plugin_tools,
        "disabled_features": DISABLED_FEATURES,
        "recovery": {
            "environment": "identical",
            "workspace": "new",
            "inputs": "declared immutable inputs re-bound; previous worker output not inherited",
        },
    })
}

/// One sentence for recover tool descriptions and responses; the JSON above is
/// the machine-readable form of the same fact.
pub const RECOVER_CHANGES: &str = "Recovery re-runs in the identical execution environment with the same capabilities; only the workspace is new. It cannot resolve a failure caused by a missing capability (for example no network); change the task's goal, inputs or explicit plugin grants instead. Delegated names remain fixed; current platform scope and plugin availability are rechecked on every call.";

/// Fixed capability policy. No caller-provided profile or fallback sandbox.
pub(crate) fn apply(
    doc: &mut DocumentMut,
    token: &str,
    socket_name: &str,
    plugin_tools: &[String],
) -> Result<()> {
    calm_types::task_execution::validate_plugin_tools(plugin_tools)
        .map_err(Error::Configuration)?;
    if token.is_empty() || socket_name.is_empty() || socket_name.contains('/') {
        return Err(Error::Configuration(
            "native MCP identity is required".into(),
        ));
    }
    doc["approval_policy"] = value("never");
    doc["default_permissions"] = value(DELIVERY_PROFILE);
    doc["web_search"] = value(WEB_SEARCH);
    doc["projects"] = Item::Table(Table::new());
    doc["projects"][WORKSPACE]["trust_level"] = value("untrusted");
    doc["shell_environment_policy"]["inherit"] = value("none");
    doc["shell_environment_policy"]["set"]["PATH"] = value(EXECUTOR_PATH);
    doc["shell_environment_policy"]["set"]["HOME"] = value("/workspace");
    doc["shell_environment_policy"]["set"]["TMPDIR"] = value("/tmp");

    // Project-local config/hooks/exec policies are untrusted. All optional routes
    // that could introduce another execution or external tool remain disabled.
    for feature in DISABLED_FEATURES {
        doc["features"][feature] = value(false);
    }
    let profile = &mut doc["permissions"][DELIVERY_PROFILE];
    profile["description"] = value("Local delivery code with private provider state excluded");
    profile["filesystem"][":root"] = value("deny");
    profile["filesystem"][":minimal"] = value("read");
    for path in ["/usr", "/bin", "/lib", "/lib64", PROVIDER_BIN] {
        profile["filesystem"][path] = value("read");
    }
    for path in WRITABLE_ROOTS {
        profile["filesystem"][path] = value("write");
    }
    for path in [
        PROVIDER_HOME,
        PROVIDER_CONTROL,
        PROVIDER_MCP,
        "/proc",
        "/workspace/.codex",
    ] {
        profile["filesystem"][path] = value("deny");
    }
    profile["network"]["enabled"] = value(NETWORK_ENABLED);
    profile["network"]["dangerously_allow_all_unix_sockets"] = value(false);
    profile["network"]["allow_local_binding"] = value(false);
    doc["mcp_servers"]["calm"]["command"] = value("/mcp-shim");
    doc["mcp_servers"]["calm"]["args"] = value(Array::new());
    doc["mcp_servers"]["calm"]["env"]["NEIGE_MCP_TOKEN"] = value(token);
    doc["mcp_servers"]["calm"]["env"]["NEIGE_MCP_SOCKET"] =
        value(format!("{PROVIDER_MCP}/{socket_name}"));
    // Native MCP still uses the existing server's card-role admission. This
    // allowlist also prevents optional forge or external-write tools appearing.
    let mut allowed = Array::new();
    for tool in MCP_TOOL_ALLOWLIST
        .iter()
        .copied()
        .chain(plugin_tools.iter().map(String::as_str))
    {
        allowed.push(tool);
    }
    doc["mcp_servers"]["calm"]["enabled_tools"] = value(allowed);
    Ok(())
}
