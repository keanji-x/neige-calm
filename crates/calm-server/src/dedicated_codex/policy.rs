use super::{Error, Result};
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub const DELIVERY_PROFILE: &str = "neige-delivery-v1";
pub const WORKSPACE: &str = "/workspace";
pub const PROVIDER_HOME: &str = "/provider/home";
pub const PROVIDER_CONTROL: &str = "/provider/control";
pub const PROVIDER_MCP: &str = "/provider/mcp";
pub const PROVIDER_BIN: &str = "/provider-bin";

/// Fixed capability policy. No caller-provided profile or fallback sandbox.
pub(crate) fn apply(doc: &mut DocumentMut, token: &str, socket_name: &str) -> Result<()> {
    if token.is_empty() || socket_name.is_empty() || socket_name.contains('/') {
        return Err(Error::Configuration(
            "native MCP identity is required".into(),
        ));
    }
    doc["approval_policy"] = value("never");
    doc["default_permissions"] = value(DELIVERY_PROFILE);
    doc["web_search"] = value("disabled");
    doc["projects"] = Item::Table(Table::new());
    doc["projects"][WORKSPACE]["trust_level"] = value("untrusted");
    doc["shell_environment_policy"]["inherit"] = value("none");
    doc["shell_environment_policy"]["set"]["PATH"] = value("/provider-bin:/usr/bin:/bin");
    doc["shell_environment_policy"]["set"]["HOME"] = value("/workspace");
    doc["shell_environment_policy"]["set"]["TMPDIR"] = value("/tmp");

    // Project-local config/hooks/exec policies are untrusted. All optional routes
    // that could introduce another execution or external tool remain disabled.
    for feature in [
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
    ] {
        doc["features"][feature] = value(false);
    }
    let profile = &mut doc["permissions"][DELIVERY_PROFILE];
    profile["description"] = value("Local delivery code with private provider state excluded");
    profile["filesystem"][":root"] = value("deny");
    profile["filesystem"][":minimal"] = value("read");
    for path in ["/usr", "/bin", "/lib", "/lib64", PROVIDER_BIN] {
        profile["filesystem"][path] = value("read");
    }
    profile["filesystem"][WORKSPACE] = value("write");
    profile["filesystem"]["/tmp"] = value("write");
    for path in [
        PROVIDER_HOME,
        PROVIDER_CONTROL,
        PROVIDER_MCP,
        "/proc",
        "/workspace/.codex",
    ] {
        profile["filesystem"][path] = value("deny");
    }
    profile["network"]["enabled"] = value(false);
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
    for tool in [
        "calm.task.complete",
        "calm.task.fail",
        "calm.report.read",
        "calm.plan.list",
    ] {
        allowed.push(tool);
    }
    doc["mcp_servers"]["calm"]["enabled_tools"] = value(allowed);
    Ok(())
}
