//! The MCP servers of a Claude task worker (#2470): the kernel's alone, as the Claude Planner's.
//! `--strict-mcp-config` keeps the owner's own servers and claude.ai connectors out of the
//! session; the kernel server is authenticated by the worker card's own token, so a call is
//! granted what the card's role is. Every spawn of a task worker's card, its first and a
//! restart's, goes through here.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{CalmError, Result};
use crate::mcp_server::McpServer;
use crate::mcp_server::wiring::{MCP_SERVER_KEY, card_mcp_env, claude_mcp_config_json};
use crate::operation::SpawnCtx;
use crate::routes::codex_cards::shell_single_quote;

/// The worker's MCP config file, beside its settings file in the card's own settings dir.
pub(crate) fn mcp_config_path(settings_path: &Path) -> Result<PathBuf> {
    Ok(super::settings_path_parent(settings_path)?.join("mcp.json"))
}

/// The command-line flags that make `mcp_config` the session's only MCP configuration and let the
/// kernel server's tools run without a permission prompt (#2509): without the rule, a call in the
/// CLI's default permission mode stops at an approval. The kernel still authorizes every call by
/// the card's role. `--allowedTools` takes a list, so an option must follow it.
pub(crate) fn mcp_flags(mcp_config: &Path) -> String {
    format!(
        " --allowedTools mcp__{MCP_SERVER_KEY} --strict-mcp-config --mcp-config {}",
        shell_single_quote(&mcp_config.to_string_lossy())
    )
}

/// At spawn: writes the config `mcp_flags` names, mints the card's token for
/// `worker_session_id`, and hands the CLI the socket and token its `${VAR}`s expand from. The same
/// pair serves the `neige` CLI the worker runs from its shell.
pub(crate) async fn wire(
    ctx: &SpawnCtx,
    mcp_server: &McpServer,
    card_id: &str,
    worker_session_id: &str,
    mcp_config: &Path,
    env: &mut Value,
) -> Result<()> {
    fs::write(
        mcp_config,
        claude_mcp_config_json(&mcp_server.shim_config.shim_bin)?,
    )
    .map_err(|e| {
        CalmError::Internal(format!(
            "write claude worker MCP config {}: {e}",
            mcp_config.display()
        ))
    })?;
    let raw_token = super::mint_claude_worker_mcp_token(ctx, card_id, worker_session_id).await?;
    let env_map = env.as_object_mut().ok_or_else(|| {
        CalmError::Internal("claude worker env must be an object before spawn".into())
    })?;
    for (key, value) in card_mcp_env(&mcp_server.shim_config.socket_path, raw_token.as_str()) {
        env_map.insert(key.into(), Value::String(value));
    }
    Ok(())
}
