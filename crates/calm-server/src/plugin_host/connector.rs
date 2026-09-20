//! Connector runtime: the client union, `secrets.json`, and tool materialization.
//! Every [`ConnectorClient`] variant is `Arc`-wrapped so a caller can clone one out from under the synchronous process-table mutex and only then `.await` on it.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::cli_query::CliQueryRuntime;
use super::http_mcp::HttpMcpClient;
use super::manifest::{CliQueryBlock, ExposedTool, McpHttpBlock, validate_connector_tool_name};
use super::mcp::McpClient;

/// File name of the per-connector secret bundle, read only by the kernel.
pub const SECRETS_FILENAME: &str = "secrets.json";

/// What a running plugin/connector talks to; `Clone` is cheap by construction (every payload is behind an `Arc`).
#[derive(Clone)]
pub enum ConnectorClient {
    /// `kind: app` — the stdio child process.
    Stdio(Arc<McpClient>),
    /// `kind: mcp-http` — remote streamable-HTTP MCP server.
    Http(Arc<HttpMcpClient>),
    /// `kind: cli-query` — a pinned local query binary; no child is supervised, each `tools/call` forks a fresh short-lived process.
    Cli(Arc<CliQueryRuntime>),
}

impl std::fmt::Debug for ConnectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No payloads: the HTTP variant holds an API key and the CLI variant holds a secret environment.
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

    /// The stdio client, or `None` for connectors; used by callers that genuinely require a `kind: app` plugin rather than widening.
    pub fn as_stdio(&self) -> Option<&Arc<McpClient>> {
        match self {
            Self::Stdio(c) => Some(c),
            _ => None,
        }
    }
}

/// Cap on `secrets.json`: it holds a handful of API keys; anything larger is a mistake or an attempt to make the kernel buffer an unbounded file.
pub const MAX_SECRETS_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A world- or group-readable secret file is not quietly accepted: enable fails and says exactly what to run.
    #[error(
        "{path} must be mode 0600 or stricter (no group/other bits), found {found:04o}; \
         run `chmod 600 {path}` and re-enable"
    )]
    BadPermissions { path: String, found: u32 },
    /// Not a regular file (a FIFO here would block the reader forever). Also reached when `open(2)` itself fails `ENXIO` on a unix-domain socket, which never survives long enough for `fstat` to classify it.
    #[error("{path} must be a regular file (found {found})")]
    NotRegularFile { path: String, found: &'static str },
    /// The `fstat` size was already over the cap. Distinct from [`Self::GrewWhileReading`] so each of the two independent size checks has an error only IT can produce.
    #[error("{path} is {size} bytes, over the {MAX_SECRETS_BYTES}-byte limit")]
    TooLarge { path: String, size: u64 },
    /// The `fstat` size was within the cap but the descriptor yielded more bytes; this is the check that actually enforces the bound, since `fstat` size is a snapshot.
    #[error(
        "{path} exceeded the {MAX_SECRETS_BYTES}-byte limit while being read \
         (it grew after its size was checked)"
    )]
    GrewWhileReading { path: String },
    #[error("{path} is not a JSON object of string values: {reason}")]
    Malformed { path: String, reason: String },
}

/// Read `<install_path>/secrets.json`. `Ok(None)` when absent; an error (never a partial map) when unreadable, wrongly permissioned, not a regular file, over [`MAX_SECRETS_BYTES`], or malformed.
/// Async on purpose: a 0600 FIFO at this path once blocked a runtime worker, and with it boot. Values go nowhere but the caller — never into the `Manifest` or any REST surface.
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

    // One open, one handle, one file: `metadata(path)` then `read_to_string(path)` re-resolves the pathname, a TOCTOU that bypassed every check at once. Everything below derives from THIS descriptor.
    // `O_NONBLOCK` makes the FIFO case a prompt refusal (a read-only FIFO open blocks until a writer appears); the open FOLLOWS symlinks on purpose so a link to a FIFO is refused as "not a regular file".
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // `open(2)` on a unix-domain socket fails ENXIO before we hold a descriptor; re-stat only to name the kind — the refusal is already decided, so this cannot be a TOCTOU on any check.
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
            return Err(SecretsError::NotRegularFile {
                path: display,
                found: match std::fs::metadata(path) {
                    Ok(m) if m.file_type().is_dir() => "a directory",
                    Ok(_) => "not a regular file",
                    Err(_) => "not a regular file",
                },
            });
        }
        Err(e) => {
            return Err(SecretsError::Io {
                path: display,
                source: e,
            });
        }
    };
    let meta = file.metadata().map_err(|e| SecretsError::Io {
        path: display.clone(),
        source: e,
    })?;

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
        // 0600-or-stricter: NOTHING outside the owner may read it; demanding exactly 0600 would reject the strictly safer 0400.
        if mode & 0o077 != 0 {
            return Err(SecretsError::BadPermissions {
                path: display,
                found: mode,
            });
        }
    }

    let buf = read_capped(&file, meta.len(), &display)?;
    let text = String::from_utf8(buf).map_err(|e| SecretsError::Malformed {
        path: display.clone(),
        reason: format!("not valid UTF-8: {e}"),
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
        if let Err(reason) = validate_secret_value(s) {
            return Err(SecretsError::Malformed {
                path: display.clone(),
                reason: format!("value of `{k}` {reason}"),
            });
        }
        out.insert(k.clone(), s.to_string());
    }
    Ok(Some(out))
}

/// The only constraint true of EVERY secret, whatever consumes it: an empty or whitespace-only value is an authoring mistake. HTTP-redaction constraints live on `HttpCredential`, since a `cli-query` secret is on no redaction path.
fn validate_secret_value(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(
            "is empty or whitespace-only; remove the key or give it a real credential".to_string(),
        );
    }
    Ok(())
}

/// The read-side half of the size cap, the one that actually ENFORCES it: `fstat` size is a snapshot and a file can grow between the stat and the read; `take(MAX + 1)` makes "one byte over" observable without buffering more.
/// A function over `impl Read` so a test can hand it a reader that yields more than `stat_len` claimed, deterministically.
fn read_capped(
    mut src: impl std::io::Read,
    stat_len: u64,
    display: &str,
) -> Result<Vec<u8>, SecretsError> {
    let mut buf = Vec::with_capacity(stat_len.min(MAX_SECRETS_BYTES) as usize);
    std::io::Read::take(&mut src, MAX_SECRETS_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| SecretsError::Io {
            path: display.to_string(),
            source: e,
        })?;
    if buf.len() as u64 > MAX_SECRETS_BYTES {
        return Err(SecretsError::GrewWhileReading {
            path: display.to_string(),
        });
    }
    Ok(buf)
}

/// Turn the complete upstream `tools/list` payload into `ExposedTool` entries: `tools_all: true` keeps every valid upstream tool, otherwise only names in `tools_allow`. An allowlisted name the server does not serve, or a name the connector rule refuses, is warned about and skipped, never fatal.
pub fn materialize_http_tools(
    plugin_id: &str,
    block: &McpHttpBlock,
    upstream: &[Value],
) -> Vec<ExposedTool> {
    let mut out = Vec::new();
    if block.tools_all {
        for tool in upstream {
            let Some(name) = tool.get("name").and_then(|name| name.as_str()) else {
                tracing::warn!(plugin_id = %plugin_id, "skipping upstream MCP tool without a string name");
                continue;
            };
            push_http_tool(&mut out, plugin_id, name, tool, "upstream tools/list name");
        }
        return out;
    }

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
        push_http_tool(&mut out, plugin_id, wanted, tool, "mcp_http.tools_allow");
    }
    out
}

fn push_http_tool(
    out: &mut Vec<ExposedTool>,
    plugin_id: &str,
    name: &str,
    tool: &Value,
    field: &str,
) {
    if let Err(e) = validate_connector_tool_name(name, field) {
        tracing::warn!(plugin_id = %plugin_id, tool = %name, error = %e, "skipping tool");
        return;
    }
    out.push(ExposedTool {
        name: name.to_string(),
        description: tool
            .get("description")
            .and_then(|description| description.as_str())
            .map(str::to_string),
        // Connector tools are ordinary calls, never forge actions: a forge action would receive the forge credential passthrough.
        kind: None,
        input_schema: tool.get("inputSchema").cloned(),
        annotations: tool.get("annotations").cloned(),
    });
}

/// The `cli-query` half: the manifest IS the catalog, so nothing can be "allowlisted but missing"; a rejected name is warned about and skipped, never fatal.
pub fn materialize_cli_tools(plugin_id: &str, block: &CliQueryBlock) -> Vec<ExposedTool> {
    let mut out = Vec::new();
    for tool in &block.tools {
        if let Err(e) = validate_connector_tool_name(&tool.name, "cli_query.tools") {
            tracing::warn!(plugin_id = %plugin_id, tool = %tool.name, error = %e, "skipping tool");
            continue;
        }
        out.push(ExposedTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            // `kind` stays `None`: a forge action would hand this tool the forge credential passthrough, which `cli-query` must never get.
            kind: None,
            input_schema: Some(tool.input_schema.clone()),
            // cli-query is read-only by contract, and Codex under `approval_policy: never` refuses every call to a tool without annotations; `report_series::resolver` reads the same `readOnlyHint` to admit a series source.
            annotations: Some(crate::mcp_server::registry::read_only_annotations()),
        });
    }
    out
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

    fn all_tools_block() -> McpHttpBlock {
        serde_json::from_value(json!({
            "url": "https://example.com/mcp",
            "tools_all": true,
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
    fn all_tools_materialization_keeps_valid_upstream_metadata_and_never_mints_forge_actions() {
        let upstream = vec![
            json!({
                "name": "list_reports",
                "description": "d",
                "inputSchema": { "type": "object" },
                "annotations": { "readOnlyHint": true }
            }),
            json!({ "name": "two words", "inputSchema": {} }),
            json!({ "description": "missing name" }),
        ];
        let tools = materialize_http_tools("c", &all_tools_block(), &upstream);
        assert_eq!(
            tools.len(),
            1,
            "invalid upstream names must still be refused"
        );
        assert_eq!(tools[0].name, "list_reports");
        assert_eq!(tools[0].description.as_deref(), Some("d"));
        assert_eq!(tools[0].input_schema, Some(json!({ "type": "object" })));
        assert_eq!(tools[0].annotations, Some(json!({ "readOnlyHint": true })));
        assert!(
            tools[0].kind.is_none(),
            "remote tools are not forge actions"
        );
    }

    #[test]
    fn a_legacy_empty_allowlist_does_not_inherit_all_tools() {
        let upstream = vec![json!({
            "name": "admin_purge",
            "inputSchema": { "type": "object" }
        })];
        let tools = materialize_http_tools("legacy", &block(&[]), &upstream);
        assert!(
            tools.is_empty(),
            "an absent/empty legacy allowlist must continue exposing nothing: {tools:?}"
        );
    }

    #[test]
    fn cli_materialization_carries_the_manifest_catalog_verbatim() {
        let block: crate::plugin_host::manifest::CliQueryBlock = serde_json::from_value(json!({
            "command": "/usr/bin/longbridge",
            "tools": [
                { "name": "quote", "description": "Get a quote",
                  "input_schema": { "type": "object",
                                    "properties": { "symbol": { "type": "string" } } },
                  "args": ["quote", "{{symbol}}"] },
                // A name the connector rule refuses: skipped, not fatal.
                { "name": "two words", "input_schema": {}, "args": [] },
            ],
        }))
        .unwrap();
        let tools = materialize_cli_tools("c", &block);
        assert_eq!(tools.len(), 1, "{tools:?}");
        assert_eq!(tools[0].name, "quote");
        assert_eq!(tools[0].description.as_deref(), Some("Get a quote"));
        assert!(
            tools[0].kind.is_none(),
            "connector tools are never forge actions"
        );
        assert!(
            tools[0]
                .input_schema
                .as_ref()
                .and_then(|s| s.pointer("/properties/symbol"))
                .is_some(),
            "the declared schema must be carried: {:?}",
            tools[0].input_schema
        );
    }

    /// Codex under `approval_policy: never` refuses every call to a tool without annotations, so every materialized cli-query tool must carry `readOnlyHint: true`.
    #[test]
    fn cli_query_tools_publish_read_only_hint_so_codex_never_asks_for_approval() {
        let block: crate::plugin_host::manifest::CliQueryBlock = serde_json::from_value(json!({
            "command": "/usr/bin/longbridge",
            "tools": [
                { "name": "quote", "description": "Get a quote",
                  "input_schema": { "type": "object" }, "args": ["quote", "{{symbol}}"] },
                { "name": "candles", "description": "Get candles",
                  "input_schema": { "type": "object" }, "args": ["candles"] },
            ],
        }))
        .unwrap();
        let tools = materialize_cli_tools("c", &block);
        assert_eq!(tools.len(), 2, "{tools:?}");
        for tool in &tools {
            assert_eq!(
                tool.annotations,
                Some(json!({ "readOnlyHint": true })),
                "cli-query tool `{}` must publish readOnlyHint: true; without it \
                 (annotations: None) Codex under approval_policy: never refuses the call (#1744)",
                tool.name
            );
        }
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
        std::fs::write(&path, r#"{"K":"sk-valid-credential"}"#).unwrap();

        for bad in [0o644, 0o640, 0o604, 0o660, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bad)).unwrap();
            let err = read_secrets(tmp.path()).await.unwrap_err();
            assert!(
                matches!(err, SecretsError::BadPermissions { found, .. } if found == bad),
                "mode {bad:04o} must be refused, got {err:?}"
            );
            assert!(err.to_string().contains("0600"), "{err}");
        }

        // 0600 and everything STRICTER must be accepted: 0400 is safer than 0600.
        for good in [0o600, 0o400, 0o200] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(good)).unwrap();
            if good == 0o200 {
                // Write-only: the read itself fails, but NOT as a permissions refusal — the mode check must have passed.
                let err = read_secrets(tmp.path()).await.unwrap_err();
                assert!(matches!(err, SecretsError::Io { .. }), "got {err:?}");
                continue;
            }
            let got = read_secrets(tmp.path()).await.unwrap().unwrap();
            assert_eq!(
                got.get("K").map(String::as_str),
                Some("sk-valid-credential"),
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

    /// One byte over the cap — the tightest input the stat check must still catch on its own.
    #[tokio::test]
    async fn a_secrets_file_one_byte_over_the_cap_is_refused() {
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

    /// An empty credential would become an empty scrub pattern, which turns `String::replace` into a memory amplifier.
    #[tokio::test]
    async fn an_empty_or_whitespace_only_secret_value_is_refused() {
        for bad in ["", "   ", "\t\n"] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(SECRETS_FILENAME);
            std::fs::write(&path, json!({ "K": bad }).to_string()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let err = read_secrets(tmp.path()).await.unwrap_err();
            assert!(
                matches!(err, SecretsError::Malformed { .. }),
                "{bad:?} must be refused, got {err:?}"
            );
            assert!(err.to_string().contains("empty"), "{err}");
        }
    }

    /// A value that is merely unusual for HTTP but valid for another consumer must NOT be refused here.
    #[tokio::test]
    async fn read_secrets_does_not_apply_http_redaction_rules_to_every_secret() {
        for value in [
            "short",                   // under the HTTP length floor
            "pass phrase with spaces", // fatal to redaction, fine in argv
            "12345678",                // all digits
            "sk-\u{4e2d}\u{6587}-key", // non-ASCII
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(SECRETS_FILENAME);
            std::fs::write(&path, json!({ "K": value }).to_string()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let got = read_secrets(tmp.path())
                .await
                .unwrap_or_else(|e| panic!("{value:?} must be readable: {e}"));
            assert_eq!(got.unwrap().get("K").map(String::as_str), Some(value));
        }
    }

    /// Witness for the **stat** check specifically: the two size checks have distinct error variants, and the `size` asserted here (the file's real length) is one the read side never sees.
    #[tokio::test]
    async fn the_stat_check_refuses_a_file_that_is_already_over_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        let len = MAX_SECRETS_BYTES as usize + 4096;
        std::fs::write(&path, vec![b'x'; len]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(
            matches!(err, SecretsError::TooLarge { size, .. } if size == len as u64),
            "the stat check must refuse this, reporting the real size: {err:?}"
        );
    }

    /// Witness for the **read-side** cap specifically: a file that grows after its size was checked, driven deterministically by a reader that yields more than the stat claimed.
    #[test]
    fn the_read_side_cap_refuses_a_file_that_grew_after_its_size_was_checked() {
        let grown = vec![b'x'; MAX_SECRETS_BYTES as usize + 1];
        let err = read_capped(&grown[..], 16, "secrets.json").unwrap_err();
        assert!(
            matches!(err, SecretsError::GrewWhileReading { .. }),
            "a descriptor yielding more than `fstat` promised must be refused: {err:?}"
        );
        // …and one byte under the cap, with the same lying stat, is fine: the rule is a size bound, not "distrust short stats".
        let ok = read_capped(&grown[..MAX_SECRETS_BYTES as usize], 16, "secrets.json").unwrap();
        assert_eq!(ok.len(), MAX_SECRETS_BYTES as usize);
    }

    /// Exactly at the cap is accepted by both checks (and then fails as non-JSON) — the boundary is `>`, not `>=`.
    #[tokio::test]
    async fn a_file_of_exactly_the_cap_is_not_refused_for_size() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        std::fs::write(&path, vec![b'x'; MAX_SECRETS_BYTES as usize]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
    }

    /// `open(2)` on a unix-domain socket fails `ENXIO` before `File::metadata` can classify it; it must still be refused as "not a regular file" rather than an opaque `Io`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_unix_socket_at_the_secrets_path_is_refused_as_not_a_regular_file() {
        // Socket dir pinned to a short base path so `$TMPDIR` length cannot break it.
        let tmp = calm_test_sockets::socket_dir("sec");
        let path = calm_test_sockets::socket_path(tmp.path(), SECRETS_FILENAME);
        let _listener = calm_test_sockets::bind(&path);
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(
            matches!(err, SecretsError::NotRegularFile { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("regular file"), "{err}");
    }

    /// A 0600 FIFO at this path must be refused promptly, not block a runtime worker (and therefore boot) forever.
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
