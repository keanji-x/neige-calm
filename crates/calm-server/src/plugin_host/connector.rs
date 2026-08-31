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
use std::io::Read as _;
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
    ///
    /// Reached from two places, because not every non-regular file survives
    /// `open(2)` long enough to be classified by `fstat`: a unix-domain socket
    /// fails the open outright with `ENXIO`. That arm re-stats the path purely
    /// to produce this error instead of a bare `Io { "No such device or
    /// address" }`, which named neither the file's kind nor what to do.
    #[error("{path} must be a regular file (found {found})")]
    NotRegularFile { path: String, found: &'static str },
    /// The `fstat` size was already over the cap. Distinct from
    /// [`Self::GrewWhileReading`] so each of the two independent size checks
    /// has an error only IT can produce — deleting either one is then
    /// observable.
    #[error("{path} is {size} bytes, over the {MAX_SECRETS_BYTES}-byte limit")]
    TooLarge { path: String, size: u64 },
    /// The `fstat` size was within the cap but the descriptor yielded more
    /// bytes. This is the check that actually enforces the bound: `fstat` size
    /// is a snapshot and a file can grow after it.
    #[error(
        "{path} exceeded the {MAX_SECRETS_BYTES}-byte limit while being read \
         (it grew after its size was checked)"
    )]
    GrewWhileReading { path: String },
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

    // ---- One open, one handle, one file. -------------------------------
    //
    // The previous shape was `metadata(path)` → checks → `read_to_string(path)`,
    // which re-resolves the PATHNAME. That is a TOCTOU: swapping what the name
    // points at between the two calls bypassed all three checks at once — a
    // FIFO stranded a blocking worker despite the `is_file()` guard, and a file
    // that grew after the stat bypassed the 64 KiB cap. Everything below is
    // derived from THIS descriptor: `File::metadata` is `fstat(2)` on it, and
    // the read goes through the same handle.
    //
    // `O_NONBLOCK` is what makes the FIFO case a prompt refusal rather than a
    // hang: opening a FIFO read-only BLOCKS until a writer appears, so the
    // `is_file()` check below would never be reached without it. It is a no-op
    // for the regular files this function is actually for.
    //
    // The open FOLLOWS symlinks on purpose (as `metadata` did): a symlink to a
    // FIFO must resolve to the FIFO and be refused as "not a regular file",
    // not silently accepted as "a symlink, fine".
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
        // Some non-regular files never reach `fstat`: `open(2)` on a
        // unix-domain socket fails with ENXIO before we hold a descriptor to
        // classify. Re-stat the path only to name the kind — the refusal is
        // already decided, so this stat cannot be a TOCTOU on any check.
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

/// Shortest credential we will accept. Not a strength requirement — it is a
/// *scrubbing* requirement: `HttpMcpClient` redacts a credential by literal
/// match, so a short value would match constantly inside unrelated upstream
/// text and turn redaction into corruption (a one-character credential of `e`
/// rewrites every `true` in the response).
pub const MIN_SECRET_LEN: usize = 8;

/// Constrain a credential at the source, as defence in depth for the redaction
/// layer in [`super::http_mcp`].
///
/// The scrubber there now parses before it redacts, so a hostile credential can
/// no longer corrupt a JSON document — but "the value cannot be authored" is a
/// stronger guarantee than "the one consumer we know about handles it", and
/// `secrets.json` is a file an operator hand-writes. Three rules:
///
/// * **non-empty / not whitespace-only** — an empty value would register `""`
///   as a scrub pattern, and `"".replace("", "<redacted>")` inserts the marker
///   at EVERY character boundary: a 4 MiB upstream error body expands by an
///   order of magnitude and is then cloned into the live status entry, the
///   broadcast `PluginState` event, and the HTTP error body;
/// * **printable ASCII, no space, no `"`, no `\`** — a credential containing a
///   newline or a control character appears JSON-*escaped* on the wire, so a
///   raw-text matcher never finds it; quote and backslash are the two
///   characters that make a value collide with JSON syntax. None of them
///   appear in any real API key;
/// * **at least [`MIN_SECRET_LEN`] characters** — see that constant.
fn validate_secret_value(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(
            "is empty or whitespace-only; remove the key or give it a real credential".to_string(),
        );
    }
    if let Some(bad) = s
        .chars()
        .find(|c| !c.is_ascii_graphic() || *c == '"' || *c == '\\')
    {
        return Err(format!(
            "contains {bad:?}, which is not allowed in a credential: values must be \
             printable ASCII with no spaces, quotes or backslashes"
        ));
    }
    if s.len() < MIN_SECRET_LEN {
        return Err(format!("is shorter than {MIN_SECRET_LEN} characters"));
    }
    Ok(())
}

/// The read-side half of the size cap — the one that actually ENFORCES it.
///
/// The `fstat` check in [`read_secrets_blocking`] is a courtesy that produces
/// the nicer error (it can name the real size) and avoids reading a
/// known-oversized file at all; but `fstat` size is a snapshot, and a file that
/// grows between the stat and the read would sail past it. `take(MAX + 1)`
/// makes "one byte over" observable without ever buffering more than that.
///
/// Split out as a function over `impl Read` for one reason: a test cannot
/// interleave a write between production's `fstat` and its read — the two are
/// adjacent, and a racing writer thread would make the test flaky rather than
/// decisive. Handing THIS function (the same code production calls, on the
/// real descriptor) a reader that yields more than `stat_len` claimed is the
/// deterministic form of exactly that file.
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

    /// One byte over the cap — the tightest input the stat check must still
    /// catch on its own. (Which check catches which case is pinned by
    /// `the_stat_check_…` / `the_read_side_cap_…` below.)
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

    /// An empty credential would become an empty scrub pattern, and an empty
    /// pattern turns `String::replace` into a memory amplifier rather than a
    /// redaction. Refused at the source.
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

    /// Defence in depth for the redaction layer (round-4 finding B). Each of
    /// these shapes is one the raw-text scrubber used to mishandle: `":` and a
    /// lone `\` corrupt JSON structure when literal-replaced, `\n` and `\x07`
    /// arrive JSON-*escaped* so a literal search never finds the decoded
    /// secret, and a short value matches inside unrelated upstream text.
    /// `http_mcp` parses before it scrubs now, but none of these can be
    /// authored either.
    #[tokio::test]
    async fn a_credential_with_json_hostile_characters_or_too_short_is_refused() {
        let cases: &[(&str, &str)] = &[
            (r#"ab":cdefgh"#, "quote"),
            (r"abc\defgh", "backslash"),
            ("abcd\nefgh", "control character / newline"),
            ("abcd\u{7}efgh", "bell"),
            ("abcd efgh", "embedded space"),
            ("sk-\u{4e2d}\u{6587}-key", "non-ASCII"),
            ("short12", "one under the length floor"),
            ("e", "single character"),
        ];
        for (bad, why) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(SECRETS_FILENAME);
            std::fs::write(&path, json!({ "K": bad }).to_string()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let err = read_secrets(tmp.path())
                .await
                .expect_err(&format!("{why}: {bad:?} must be refused"));
            assert!(
                matches!(err, SecretsError::Malformed { .. }),
                "{why}: got {err:?}"
            );
        }
    }

    /// The rule stated positively, so the test above cannot pass by refusing
    /// everything: a real-shaped API key is accepted, and exactly the length
    /// floor is the boundary (`<`, not `<=`).
    #[tokio::test]
    async fn a_well_formed_credential_is_accepted_at_and_above_the_length_floor() {
        for good in [
            "a".repeat(MIN_SECRET_LEN),
            "sk-super-secret-8213".to_string(),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join(SECRETS_FILENAME);
            std::fs::write(&path, json!({ "K": &good }).to_string()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let got = read_secrets(tmp.path()).await.expect("must be accepted");
            assert_eq!(got.unwrap().get("K"), Some(&good));
        }
    }

    /// Witness for the **stat** check specifically.
    ///
    /// Round-3 finding: the previous version of this test asserted only
    /// "TooLarge or not", which BOTH checks can satisfy — deleting either one
    /// left it green, while its comment claimed it pinned both. The two now
    /// have distinct error variants, so this fixture can only be satisfied by
    /// the stat check: delete it and the same file is refused by the read side
    /// as `GrewWhileReading`, and the `size` this asserts (the file's real
    /// length, which the read side never sees) is gone with it.
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

    /// Witness for the **read-side** cap specifically: the case it exists for
    /// is a file that grows after its size was checked.
    ///
    /// Driving that through a real file would need a write interleaved between
    /// production's `fstat` and its read — two adjacent statements, so a
    /// racing writer makes a flaky test rather than a decisive one. This calls
    /// the same production function on a reader that yields more than the stat
    /// claimed, which is that file, deterministically. Delete the
    /// `buf.len() > MAX` refusal in `read_capped` and this goes green-to-red.
    #[test]
    fn the_read_side_cap_refuses_a_file_that_grew_after_its_size_was_checked() {
        let grown = vec![b'x'; MAX_SECRETS_BYTES as usize + 1];
        let err = read_capped(&grown[..], 16, "secrets.json").unwrap_err();
        assert!(
            matches!(err, SecretsError::GrewWhileReading { .. }),
            "a descriptor yielding more than `fstat` promised must be refused: {err:?}"
        );
        // …and one byte under the cap, with the same lying stat, is fine: the
        // rule is a size bound, not "distrust short stats".
        let ok = read_capped(&grown[..MAX_SECRETS_BYTES as usize], 16, "secrets.json").unwrap();
        assert_eq!(ok.len(), MAX_SECRETS_BYTES as usize);
    }

    /// Exactly at the cap is accepted by both checks (and then fails as
    /// non-JSON) — the boundary is `>`, not `>=`.
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

    /// `open(2)` on a unix-domain socket fails `ENXIO` before `File::metadata`
    /// can classify it, so the descriptor-based check never runs. It must
    /// still be refused as "not a regular file" rather than as an opaque
    /// `Io { "No such device or address" }`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_unix_socket_at_the_secrets_path_is_refused_as_not_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SECRETS_FILENAME);
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let err = read_secrets(tmp.path()).await.unwrap_err();
        assert!(
            matches!(err, SecretsError::NotRegularFile { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("regular file"), "{err}");
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
