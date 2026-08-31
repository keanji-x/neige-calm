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
//! * [`materialize_http_tools`] — §2.7's synthesis of `ExposedTool` entries for
//!   a connector whose tool catalog does not live in `exposes_tools`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::http_mcp::HttpMcpClient;
use super::manifest::{ExposedTool, McpHttpBlock, validate_connector_tool_name};
use super::mcp::McpClient;

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
    // NOTE: there is deliberately no `Cli` variant. `kind: cli-query` parses
    // and validates in this slice but its enable path fails before any runtime
    // is constructed (#1164 P3), so a variant here would be a state no code
    // path can reach — dead weight that a reader would mistake for a live
    // capability. P3 reintroduces it together with the executor.
}

impl std::fmt::Debug for ConnectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No payloads: the HTTP variant holds an API key and the CLI variant
        // holds a secret environment.
        f.write_str(match self {
            Self::Stdio(_) => "ConnectorClient::Stdio",
            Self::Http(_) => "ConnectorClient::Http",
        })
    }
}

impl ConnectorClient {
    /// Short wire-ish label for logs and error messages.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::Http(_) => "mcp-http",
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
// secrets.json (§2.4)
// ---------------------------------------------------------------------------

/// Cap on `secrets.json`. It holds a handful of API keys; anything larger is a
/// mistake or an attempt to make the kernel buffer an unbounded file.
pub const MAX_SECRETS_BYTES: u64 = 64 * 1024;

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
        "{path} must be mode 0600 or stricter (no group/other bits), found {found:04o}; \
         run `chmod 600 {path}` and re-enable"
    )]
    BadPermissions { path: String, found: u32 },
    /// Not a regular file. A FIFO here would block the reader forever — and
    /// before this slice that reader was an async runtime worker.
    #[error("{path} must be a regular file (found {found})")]
    NotRegularFile { path: String, found: &'static str },
    #[error("{path} is {size} bytes, over the {MAX_SECRETS_BYTES}-byte limit")]
    TooLarge { path: String, size: u64 },
    #[error("{path} is not a JSON object of string values: {reason}")]
    Malformed { path: String, reason: String },
}

/// Read `<install_path>/secrets.json`.
///
/// Returns `Ok(None)` when the file is absent — a connector with no
/// credentials is legal. Returns an error (never a partial map) when the file
/// exists but is unreadable, wrongly permissioned, not a regular file, over
/// [`MAX_SECRETS_BYTES`], or malformed.
///
/// **Async on purpose.** The synchronous `metadata` + `read_to_string` this
/// replaced ran directly on the spawn path, which `AppState::new` awaits
/// inline: a 0600 FIFO at that path blocked a runtime worker and, with it,
/// boot. `spawn_blocking` + the regular-file check close both halves.
///
/// Values are returned to the caller and go nowhere else: they are not merged
/// into the `Manifest`, so they cannot reach `GET /api/plugins/{id}` (which
/// serves the DB row's manifest blob) or any other REST surface.
pub async fn read_secrets(
    install_path: &Path,
) -> Result<Option<BTreeMap<String, String>>, SecretsError> {
    let path = install_path.join(SECRETS_FILENAME);
    tokio::task::spawn_blocking(move || read_secrets_blocking(&path))
        .await
        .unwrap_or_else(|e| {
            Err(SecretsError::Io {
                path: SECRETS_FILENAME.to_string(),
                source: std::io::Error::other(format!("secrets read task failed: {e}")),
            })
        })
}

fn read_secrets_blocking(path: &Path) -> Result<Option<BTreeMap<String, String>>, SecretsError> {
    let display = path.display().to_string();

    // `metadata` (not `symlink_metadata`) on purpose: it FOLLOWS symlinks, so
    // a symlink pointing at a FIFO resolves to the FIFO and is caught by the
    // `is_file()` check below rather than passing as "a symlink, fine".
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(SecretsError::Io {
                path: display,
                source: e,
            });
        }
    };

    if !meta.is_file() {
        return Err(SecretsError::NotRegularFile {
            path: display,
            found: if meta.is_dir() {
                "a directory"
            } else {
                "not a regular file"
            },
        });
    }
    if meta.len() > MAX_SECRETS_BYTES {
        return Err(SecretsError::TooLarge {
            path: display,
            size: meta.len(),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        // 0600-or-stricter: the requirement is that NOTHING outside the owner
        // can read it. Demanding exactly 0600 rejected the strictly safer
        // 0400, which is a refusal with no security story behind it.
        if mode & 0o077 != 0 {
            return Err(SecretsError::BadPermissions {
                path: display,
                found: mode,
            });
        }
    }

    let text = std::fs::read_to_string(path).map_err(|e| SecretsError::Io {
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

// NOTE: there is no `materialize_cli_tools` here. `kind: cli-query` has no
// enable path that reaches materialization in this slice, so the function had
// no production caller — only its own test. #1164 P3 adds it alongside the
// executor that would actually call it.

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
    #[tokio::test]
    async fn secrets_reject_group_or_other_readable_but_accept_stricter_than_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        std::fs::write(&path, r#"{"K":"v"}"#).unwrap();

        for bad in [0o644, 0o640, 0o604, 0o660, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bad)).unwrap();
            let err = read_secrets(tmp.path()).await.unwrap_err();
            assert!(
                matches!(err, SecretsError::BadPermissions { found, .. } if found == bad),
                "mode {bad:04o} must be refused, got {err:?}"
            );
            assert!(err.to_string().contains("0600"), "{err}");
        }

        // 0600 and everything STRICTER must be accepted: 0400 is safer than
        // 0600, and refusing it would be a rule with no security story.
        for good in [0o600, 0o400, 0o200] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(good)).unwrap();
            if good == 0o200 {
                // Write-only: the read itself fails, but NOT as a permissions
                // refusal — the mode check must have passed.
                let err = read_secrets(tmp.path()).await.unwrap_err();
                assert!(matches!(err, SecretsError::Io { .. }), "got {err:?}");
                continue;
            }
            let got = read_secrets(tmp.path()).await.unwrap().unwrap();
            assert_eq!(
                got.get("K").map(String::as_str),
                Some("v"),
                "mode {good:04o} must be accepted"
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[tokio::test]
    async fn missing_secrets_file_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_secrets(tmp.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_directory_at_the_secrets_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(SECRETS_FILENAME)).unwrap();
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(
            matches!(err, SecretsError::NotRegularFile { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_oversized_secrets_file_is_refused_without_being_buffered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        std::fs::write(&path, vec![b'x'; MAX_SECRETS_BYTES as usize + 1]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(matches!(err, SecretsError::TooLarge { .. }), "{err:?}");
    }

    /// The whole reason `read_secrets` moved to `spawn_blocking` + a
    /// regular-file check: a 0600 FIFO at this path used to block a runtime
    /// worker (and therefore boot) forever. It must now be refused promptly.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fifo_at_the_secrets_path_is_refused_and_does_not_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: plain libc call on a path inside a fresh temp dir.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let err = tokio::time::timeout(std::time::Duration::from_secs(5), read_secrets(tmp.path()))
            .await
            .expect("read_secrets must not block on a FIFO")
            .unwrap_err();
        assert!(
            matches!(err, SecretsError::NotRegularFile { .. }),
            "{err:?}"
        );
    }
}
