//! UDS listener + per-connection JSON-RPC pump for the kernel-as-MCP-server.
//!
//! PR7a (#136). One Unix domain socket lives under
//! `<config.data_dir>/mcp/kernel.sock` (mode 0600). Each `accept()` spawns
//! a `tokio` task that:
//!
//!   1. Reads line-delimited JSON frames from the socket via
//!      [`crate::mcp_server::framing::parse_frame`].
//!   2. Waits for the first `initialize` request, drives
//!      [`crate::mcp_server::handshake::handle_initialize`] to bind a
//!      [`CardIdentity`] to the connection, and sends the response.
//!   3. After handshake, treats every subsequent `tools/call` as an
//!      invocation of a [`ToolRegistry`] handler, passing the pinned
//!      [`CardIdentity`].
//!   4. Responds to `tools/list` from the registry's descriptors.
//!   5. Echoes a `MethodNotFound` for any other request method.
//!
//! ## Lifecycle
//!
//! [`McpServer::spawn`] binds the socket and returns immediately; the
//! `accept` loop runs in a background tokio task held alive by the
//! `Arc<McpServer>` field on [`crate::state::AppState`]. Dropping the
//! `AppState` doesn't immediately abort the listener — closure happens
//! when the task's stop-channel fires (today: process exit). PR8 may
//! add a graceful-shutdown signal once `wait_for_events` introduces
//! long-poll handlers that need cooperative cancellation.
//!
//! ## Why not axum / hyper
//!
//! MCP is line-delimited JSON-RPC, not HTTP. The transport is a few
//! hundred lines of `tokio::net::UnixListener` + `BufReader::lines()`;
//! adding an HTTP framework would only obscure the framing.

use crate::card_role_cache::CardRoleCache;
use crate::db::RouteRepo;
use crate::event_cursor::EventCursorCache;
use crate::mcp_server::framing::{
    Frame, RequestId, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};
use crate::mcp_server::handshake::handle_initialize;
use crate::mcp_server::registry::{AppContext, CardIdentity, ToolHandler, ToolRegistry};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

/// Protocol version the kernel advertises in its `initialize` response.
/// Codex's MCP client echoes back whatever it sent; we don't strictly
/// validate the request's version yet (PR7a is the first wire we ship),
/// but we *do* echo a stable value so future codex versions can match
/// behavior on it.
pub const KERNEL_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// File mode the listener applies to the socket after `bind`. Matches
/// the trust model in `auth.rs`: the per-card token is the credential,
/// and the socket's filesystem ACL is the perimeter — only the same
/// uid (i.e. processes the kernel itself spawned) can `connect`.
const SOCKET_MODE: u32 = 0o600;

/// Configuration the codex daemon needs to know about the kernel's MCP
/// server. Threaded into `build_codex_config_toml_with_prompt` so each
/// Spec/Worker `$CODEX_HOME/config.toml` gets a `[mcp_servers.calm]`
/// block pointing at the right socket + shim binary.
///
/// `Clone` is cheap (two small `PathBuf`s).
#[derive(Clone, Debug)]
pub struct McpShimConfig {
    /// Path to the `neige-mcp-stdio-shim` binary, resolved at boot.
    pub shim_bin: PathBuf,
    /// UDS the shim should `connect()` to.
    pub socket_path: PathBuf,
}

/// Handle held on [`crate::state::AppState`]. Owns the listener task's
/// `JoinHandle` (held via `Mutex<Option<…>>` so a future shutdown path
/// can `take()` and `abort()` it), plus the shim config the spec_card
/// helper reads to build per-card config.toml blocks.
pub struct McpServer {
    pub shim_config: McpShimConfig,
    #[allow(dead_code)]
    listener_task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl McpServer {
    /// Bind the UDS at `socket_path`, spawn the accept loop, and return
    /// the handle. The accept loop runs until the process exits or the
    /// listener errors out (logged at warn!).
    ///
    /// On `bind` failure, the socket is reset and re-bound: a stale
    /// socket file from a prior crashed boot would otherwise leave the
    /// listener stuck on `EADDRINUSE`. (Matches the pattern in
    /// `routes/terminal.rs`'s daemon socket setup.)
    pub async fn spawn(
        repo: Arc<dyn RouteRepo>,
        events: crate::event::EventBus,
        card_role_cache: CardRoleCache,
        event_cursor_cache: EventCursorCache,
        socket_path: PathBuf,
        shim_bin: PathBuf,
        registry: Arc<ToolRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = socket_path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("mkdir mcp socket dir {}: {e}", parent.display()))?;
        }
        // Unlink any stale socket from a prior boot. Ignore "no such
        // file" — that's the fresh-install path.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| anyhow::anyhow!("bind mcp socket {}: {e}", socket_path.display()))?;

        // Tighten the perm bits — the default umask leaves a
        // world-readable socket, which would let a different user
        // poke at the kernel's MCP wire.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(SOCKET_MODE);
            std::fs::set_permissions(&socket_path, perms)
                .map_err(|e| anyhow::anyhow!("chmod mcp socket {}: {e}", socket_path.display()))?;
        }

        let ctx = Arc::new(AppContext {
            repo,
            events,
            card_role_cache,
            event_cursor_cache,
        });

        let socket_for_handle = socket_path.clone();
        let task = tokio::spawn(accept_loop(listener, ctx, registry, socket_for_handle));

        tracing::info!(
            socket = %socket_path.display(),
            "mcp_server: kernel-as-MCP-server listening"
        );

        Ok(Arc::new(Self {
            shim_config: McpShimConfig {
                shim_bin,
                socket_path,
            },
            listener_task: std::sync::Mutex::new(Some(task)),
        }))
    }
}

async fn accept_loop(
    listener: UnixListener,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    socket_path: PathBuf,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ctx = ctx.clone();
                let registry = registry.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, ctx, registry).await {
                        tracing::warn!(error = %e, "mcp_server: connection handler errored");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(
                    socket = %socket_path.display(),
                    error = %e,
                    "mcp_server: accept failed; sleeping 100ms before retry"
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// One per-connection task. Owns the socket; runs until either side
/// hangs up or a fatal framing error happens.
async fn handle_connection(
    stream: UnixStream,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
) -> anyhow::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();

    // Phase 1: wait for `initialize`. Anything else before initialize
    // gets a `MethodNotFound`-shaped error. We don't bind an identity
    // until `initialize` succeeds — every other request before then is
    // unauthenticated and rejected.
    let identity = loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF before initialize — client disconnected.
            return Ok(());
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let frame = match parse_frame(trimmed) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(line = %trimmed, error = %e, "mcp_server: invalid frame pre-initialize");
                continue;
            }
        };
        match frame {
            Frame::Request { id, method, params } if method == "initialize" => {
                match handle_initialize(
                    ctx.repo.as_ref(),
                    &ctx.card_role_cache,
                    &params,
                    KERNEL_MCP_PROTOCOL_VERSION,
                )
                .await
                {
                    Ok(ok) => {
                        let frame = build_ok_response_frame(&id, &ok.result_payload);
                        wr.write_all(&frame).await?;
                        wr.flush().await?;
                        break ok.identity;
                    }
                    Err(rpc_err) => {
                        let frame = build_error_response_frame(&id, &rpc_err);
                        wr.write_all(&frame).await?;
                        wr.flush().await?;
                        // The client should disconnect on
                        // initialize failure. We drop the connection.
                        return Ok(());
                    }
                }
            }
            Frame::Request { id, method, .. } => {
                // Pre-initialize traffic — refuse.
                let err = RpcError::custom(
                    -32002,
                    format!("server not initialized; expected `initialize`, got `{method}`"),
                );
                let frame = build_error_response_frame(&id, &err);
                wr.write_all(&frame).await?;
                wr.flush().await?;
                return Ok(());
            }
            Frame::Notification { method, .. } => {
                tracing::debug!(method = %method, "mcp_server: pre-initialize notification dropped");
            }
            Frame::Response { .. } => {
                // We don't issue requests pre-handshake; a response
                // arriving here is wrong-direction noise.
            }
        }
    };

    tracing::info!(
        card_id = %identity.card_id.as_str(),
        role = ?identity.role,
        "mcp_server: connection bound to card identity"
    );

    // Phase 2: post-initialize message pump. Any request after this
    // sees the pinned `identity` value.
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let frame = match parse_frame(trimmed) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(line = %trimmed, error = %e, "mcp_server: invalid frame post-initialize");
                continue;
            }
        };

        match frame {
            Frame::Request { id, method, params } => {
                let resp = dispatch_request(&id, &method, params, &ctx, &identity, &registry).await;
                let bytes = match resp {
                    Ok(value) => build_ok_response_frame(&id, &value),
                    Err(err) => build_error_response_frame(&id, &err),
                };
                wr.write_all(&bytes).await?;
                wr.flush().await?;
            }
            Frame::Notification { method, .. } => {
                // PR7a's tools are all request/response. Cancellation
                // / progress notifications are PR8 territory — drop
                // for now.
                tracing::debug!(method = %method, "mcp_server: notification dropped (PR7a no-op)");
            }
            Frame::Response { id, .. } => {
                tracing::debug!(?id, "mcp_server: stray response from client (ignored)");
            }
        }
    }
}

/// Dispatch a single JSON-RPC request to the right place. Centralized
/// here so the pre-/post-initialize message pump can share a single
/// switch.
async fn dispatch_request(
    _id: &RequestId,
    method: &str,
    params: Value,
    ctx: &Arc<AppContext>,
    identity: &CardIdentity,
    registry: &Arc<ToolRegistry>,
) -> Result<Value, RpcError> {
    match method {
        "tools/list" => {
            // Codex's `tools/list` expects `{ "tools": [...] }`. Each
            // entry is `{ name, description, inputSchema }`.
            let tools: Vec<Value> = registry
                .descriptors()
                .into_iter()
                .map(|d| {
                    json!({
                        "name": d.name,
                        "description": d.description,
                        "inputSchema": d.input_schema,
                    })
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| RpcError::invalid_params("tools/call: missing `name`"))?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let handler: ToolHandler = registry
                .lookup(name)
                .ok_or_else(|| RpcError::method_not_found(&format!("tools/call: {name}")))?;
            let fut = handler(ctx.clone(), identity.clone(), arguments);
            let raw = fut.await?;
            // Wrap the handler's raw payload in the MCP `CallToolResult`
            // envelope so codex's MCP client parses it. The kernel's
            // tools today return a JSON object; we surface it as a
            // single `text` content block + `structuredContent` field
            // so downstream agents can either parse the structured form
            // or read the text representation.
            let text = serde_json::to_string(&raw).unwrap_or_else(|_| "{}".to_string());
            Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": raw,
                "isError": false,
            }))
        }
        other => Err(RpcError::method_not_found(other)),
    }
}

/// Helper used by integration tests to resolve the kernel-side socket
/// path from a data dir, matching what the production code uses. Kept
/// `pub(crate)` so it can't be reached from outside the crate's test
/// module set.
///
/// PR7a (#136) — integration tests for the MCP server are deferred to
/// followup PR7a.1; the helper lands now so the followup can use it
/// without churning this file again. `dead_code` allow until then.
#[allow(dead_code)]
pub(crate) fn default_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("mcp").join("kernel.sock")
}
