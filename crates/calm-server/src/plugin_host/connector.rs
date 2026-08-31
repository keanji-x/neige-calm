//! #1164 — connector runtime: the client union, `secrets.json`, and tool
//! materialization.
//!
//! Three pieces live here because they are the only things a non-`app`
//! connector needs that an `app` plugin does not:
//!
//! * [`ConnectorClient`] — the union that replaced `RunningPlugin.mcp`'s
//!   `Arc<McpClient>` (§2.2 / D8). Every variant is `Arc`-wrapped so a caller
//!   can clone one out from under the *synchronous* process-table mutex and
//!   only then `.await` on it. Holding that lock across an await is a deadlock
//!   the existing code is careful to avoid (`plugin_host::mod` §"Process
//!   table"), and a non-`Clone` client would have forced exactly that.
//! * [`read_secrets`] — §2.4's `secrets.json`. No DB table in v0.
//! * the `materialize_*` helpers — §2.7's synthesis of `ExposedTool` entries
//!   for connectors whose tool catalog does not live in `exposes_tools`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::http_mcp::HttpMcpClient;
use super::manifest::{
    CliQueryBlock, ExposedTool, ManifestError, McpHttpBlock, validate_connector_tool_name,
};
use super::mcp::{CallToolResult, McpClient, RpcError};

/// File name of the per-connector secret bundle, read only by the kernel.
pub const SECRETS_FILENAME: &str = "secrets.json";

// ---------------------------------------------------------------------------
// ConnectorClient
// ---------------------------------------------------------------------------

/// What a running plugin/connector talks to.
///
/// `Clone` is cheap by construction (every payload is behind an `Arc`) — see
/// the module header for why that is a hard requirement rather than a
/// convenience.
#[derive(Clone)]
pub enum ConnectorClient {
    /// `kind: app` — today's stdio child process. Behaviour unchanged.
    Stdio(Arc<McpClient>),
    /// `kind: mcp-http` — remote streamable-HTTP MCP server.
    Http(Arc<HttpMcpClient>),
    /// `kind: cli-query` — pinned local query CLI.
    Cli(Arc<CliQueryRuntime>),
}

impl std::fmt::Debug for ConnectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No payloads: the HTTP variant holds an API key and the CLI variant
        // holds a secret environment.
        f.write_str(match self {
            Self::Stdio(_) => "ConnectorClient::Stdio",
            Self::Http(_) => "ConnectorClient::Http",
            Self::Cli(_) => "ConnectorClient::Cli",
        })
    }
}

impl ConnectorClient {
    /// Short wire-ish label for logs and error messages.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::Http(_) => "mcp-http",
            Self::Cli(_) => "cli-query",
        }
    }

    /// The stdio client, or `None` for connectors. Callers that genuinely
    /// require a `kind: app` plugin (forge-action dispatch, card creation via
    /// tool call, `neige.*` callbacks) use this rather than widening.
    pub fn as_stdio(&self) -> Option<&Arc<McpClient>> {
        match self {
            Self::Stdio(c) => Some(c),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// cli-query runtime (execution lands in a later slice — see §7 P3)
// ---------------------------------------------------------------------------

/// Everything a `cli-query` connector needs at call time: the pinned absolute
/// command path, its fingerprint, the child environment, and the declared tool
/// table.
///
/// #1164 P1 defines and validates the shape; the resolve/pin/exec runtime is
/// the next slice. Construction is therefore only exercised by tests today,
/// and [`Self::tools_call`] returns a clear "not implemented" rather than
/// pretending.
pub struct CliQueryRuntime {
    pub plugin_id: String,
    /// Absolute path resolved once at enable time (§2.3). Empty until the
    /// execution slice lands.
    pub command_path: std::path::PathBuf,
    /// `<command> --version` first line, or a size+mtime stamp on failure.
    pub fingerprint: String,
    /// Fully-built child environment: `env_clear()` + base set + `env_allow`
    /// + `secret_env`. Never logged.
    pub env: BTreeMap<String, String>,
    pub block: CliQueryBlock,
}

impl CliQueryRuntime {
    pub async fn tools_call(
        &self,
        name: &str,
        _arguments: Value,
    ) -> Result<CallToolResult, RpcError> {
        Err(RpcError::custom(
            -32002,
            format!(
                "cli-query connector `{}` cannot run `{name}` yet: \
                 the execution runtime is not implemented in this slice (#1164 P3)",
                self.plugin_id
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// secrets.json (§2.4)
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The refusal that matters: a world- or group-readable secret file is not
    /// quietly accepted. Enable fails and says exactly what to run.
    #[error(
        "{path} must be mode 0600 (owner read/write only), found {found:04o}; \
         run `chmod 600 {path}` and re-enable"
    )]
    BadPermissions { path: String, found: u32 },
    #[error("{path} is not a JSON object of string values: {reason}")]
    Malformed { path: String, reason: String },
}

/// Read `<install_path>/secrets.json`.
///
/// Returns `Ok(None)` when the file is absent — a connector with no
/// credentials is legal. Returns an error (never a partial map) when the file
/// exists but is unreadable, wrongly permissioned, or malformed.
///
/// Values are returned to the caller and go nowhere else: they are not merged
/// into the `Manifest`, so they cannot reach `GET /api/plugins/{id}` (which
/// serves the DB row's manifest blob) or any other REST surface.
pub fn read_secrets(install_path: &Path) -> Result<Option<BTreeMap<String, String>>, SecretsError> {
    let path = install_path.join(SECRETS_FILENAME);
    let display = path.display().to_string();

    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(SecretsError::Io {
                path: display,
                source: e,
            });
        }
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(SecretsError::BadPermissions {
                path: display,
                found: mode,
            });
        }
    }
    #[cfg(not(unix))]
    let _ = &meta;

    let text = std::fs::read_to_string(&path).map_err(|e| SecretsError::Io {
        path: display.clone(),
        source: e,
    })?;
    let parsed: Value = serde_json::from_str(&text).map_err(|e| SecretsError::Malformed {
        path: display.clone(),
        reason: e.to_string(),
    })?;
    let obj = parsed.as_object().ok_or_else(|| SecretsError::Malformed {
        path: display.clone(),
        reason: "top level must be an object".to_string(),
    })?;

    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let s = v.as_str().ok_or_else(|| SecretsError::Malformed {
            path: display.clone(),
            reason: format!("value of `{k}` must be a string"),
        })?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// Tool materialization (§2.7)
// ---------------------------------------------------------------------------

/// Turn an upstream `tools/list` payload into `ExposedTool` entries, keeping
/// only names in `tools_allow`.
///
/// An allowlisted name the server does not serve is warned about and skipped —
/// one stale entry must not take the whole connector down (§2.2).
///
/// There is no `ExposedTool::validate` in the tree (`Manifest::validate` only
/// covers views / workflows / permissions / entrypoint), so the name check is
/// applied here explicitly; a rejected name is skipped, not fatal.
pub fn materialize_http_tools(
    plugin_id: &str,
    block: &McpHttpBlock,
    upstream: &[Value],
) -> Vec<ExposedTool> {
    let mut out = Vec::new();
    for wanted in &block.tools_allow {
        let Some(tool) = upstream
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(wanted.as_str()))
        else {
            tracing::warn!(
                plugin_id = %plugin_id,
                tool = %wanted,
                "tools_allow names a tool the upstream MCP server does not serve — ignoring"
            );
            continue;
        };
        if let Err(e) = validate_connector_tool_name(wanted, "mcp_http.tools_allow") {
            tracing::warn!(plugin_id = %plugin_id, tool = %wanted, error = %e, "skipping tool");
            continue;
        }
        out.push(ExposedTool {
            name: wanted.clone(),
            description: tool
                .get("description")
                .and_then(|d| d.as_str())
                .map(str::to_string),
            // `kind` stays `None`: connector tools are ordinary tool calls,
            // never forge actions (D6 — a forge action would hand them the
            // forge credential passthrough).
            kind: None,
            input_schema: tool.get("inputSchema").cloned(),
            annotations: tool.get("annotations").cloned(),
        });
    }
    out
}

/// Turn a validated `cli_query.tools` table into `ExposedTool` entries.
pub fn materialize_cli_tools(block: &CliQueryBlock) -> Result<Vec<ExposedTool>, ManifestError> {
    let mut out = Vec::with_capacity(block.tools.len());
    for (i, tool) in block.tools.iter().enumerate() {
        validate_connector_tool_name(&tool.name, &format!("cli_query.tools[{i}].name"))?;
        out.push(ExposedTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            kind: None,
            input_schema: Some(tool.input_schema.clone()),
            annotations: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(tools_allow: &[&str]) -> McpHttpBlock {
        serde_json::from_value(json!({
            "url": "https://example.com/mcp",
            "tools_allow": tools_allow,
        }))
        .unwrap()
    }

    #[test]
    fn http_materialization_filters_by_allowlist() {
        let upstream = vec![
            json!({ "name": "list_reports", "description": "d",
                    "inputSchema": { "type": "object" } }),
            json!({ "name": "secret_admin_tool" }),
        ];
        let tools = materialize_http_tools("c", &block(&["list_reports"]), &upstream);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "list_reports");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(tools[0].input_schema, Some(json!({ "type": "object" })));
    }

    #[test]
    fn allowlisted_but_missing_upstream_tool_is_skipped_not_fatal() {
        let upstream = vec![json!({ "name": "present" })];
        let tools = materialize_http_tools("c", &block(&["present", "gone"]), &upstream);
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["present"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn secrets_require_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        std::fs::write(&path, r#"{"K":"v"}"#).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_secrets(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SecretsError::BadPermissions { found: 0o644, .. }),
            "got {err:?}"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let got = read_secrets(tmp.path()).unwrap().unwrap();
        assert_eq!(got.get("K").map(String::as_str), Some("v"));
    }

    #[test]
    fn missing_secrets_file_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_secrets(tmp.path()).unwrap().is_none());
    }
}
