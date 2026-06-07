//! UDS listener + per-connection JSON-RPC pump for the kernel-as-MCP-server.
//!
//! PR7a (#136). One Unix domain socket lives under
//! `<config.data_dir>/mcp/kernel.sock` (mode 0600). Each `accept()` spawns
//! a `tokio` task that:
//!
//!   1. Reads line-delimited JSON frames from the socket via
//!      [`crate::mcp_server::framing::parse_frame`].
//!   2. Waits for the first `initialize` request, drives
//!      [`crate::mcp_server::handshake::handle_initialize`] to verify
//!      daemon-level trust, and sends the response.
//!   3. After handshake, treats every subsequent `tools/call` as an
//!      invocation of a [`ToolRegistry`] handler, resolving identity
//!      from per-call `_meta.threadId`.
//!   4. Responds to `tools/list` from the registry's descriptors.
//!   5. Echoes a `MethodNotFound` for any other request method.
//!
//! ## Lifecycle
//!
//! [`McpServer::spawn`] binds the socket and returns immediately; the
//! `accept` loop runs in a background tokio task held alive by the
//! `Arc<McpServer>` field on [`crate::state::AppState`]. Dropping the
//! `AppState` doesn't immediately abort the listener — closure happens
//! when the task's stop-channel fires (today: process exit). A future
//! graceful-shutdown signal could be added here if a long-running handler
//! ever needs cooperative cancellation.
//!
//! ## Why not axum / hyper
//!
//! MCP is line-delimited JSON-RPC, not HTTP. The transport is a few
//! hundred lines of `tokio::net::UnixListener` + `BufReader::lines()`;
//! adding an HTTP framework would only obscure the framing.

use crate::db::RouteRepo;
use crate::mcp_server::framing::{
    Frame, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};
use crate::mcp_server::handshake::handle_initialize;
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ToolCallIdentity, ToolHandler, ToolRegistry,
};
use crate::runtime_lookup::resolve_card_for_thread as resolve_card_for_thread_runtime;
use crate::runtime_repo::AgentProvider;
use crate::state::WriteContext;
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

/// Boot-time probe budget for the "is there already a live listener at
/// this path?" check. UDS connects are sub-ms on a healthy listener;
/// the budget exists only to bound a pathological case where the kernel
/// stalls a connect attempt. A timeout falls through to the stale-file
/// reclaim path, same as `ECONNREFUSED`.
const LIVE_LISTENER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Configuration the codex daemon needs to know about the kernel's MCP
/// server, including the shim binary and Unix socket path.
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
    /// If `socket_path` already exists we probe it with a short
    /// `UnixStream::connect`. A live peer means another process is
    /// already serving on the same XDG-shared path (a second
    /// `calm-server` against the same `$HOME`, a leftover from a prior
    /// boot, etc.); we refuse to boot rather than unlink-and-rebind,
    /// because the unlink would steal the path from the live listener
    /// without breaking its socket — `connect()` against the new file
    /// would then return `ECONNREFUSED` even though the kernel thinks
    /// it's "running". A connect failure (`ECONNREFUSED` /
    /// `ENOENT`) is the stale-file case the original boot code was
    /// already handling; we unlink and rebind in that path the way
    /// `routes/terminal.rs`'s daemon-socket setup does.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        repo: Arc<dyn RouteRepo>,
        events: crate::event::EventBus,
        write: WriteContext,
        socket_path: PathBuf,
        shim_bin: PathBuf,
        registry: Arc<ToolRegistry>,
        daemon_token_hash: Option<String>,
    ) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = socket_path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("mkdir mcp socket dir {}: {e}", parent.display()))?;
        }
        if socket_path.exists() {
            // Probe before unlink — see doc above.
            match tokio::time::timeout(
                LIVE_LISTENER_PROBE_TIMEOUT,
                UnixStream::connect(&socket_path),
            )
            .await
            {
                Ok(Ok(_stream)) => {
                    anyhow::bail!(
                        "another process is already listening on mcp socket {} \
                         (refusing to unlink-and-rebind; co-tenant calm-server on the same data dir?)",
                        socket_path.display()
                    );
                }
                Ok(Err(_)) | Err(_) => {
                    // Connect refused, timed out, or other error — treat as
                    // stale and reclaim the path.
                    let _ = std::fs::remove_file(&socket_path);
                }
            }
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
            write,
            daemon_token_hash,
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
    let (daemon_trust, legacy_identity) = loop {
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
            Frame::Request {
                id, method, params, ..
            } if method == "initialize" => {
                match handle_initialize(
                    ctx.repo.as_ref(),
                    ctx.daemon_token_hash.as_deref(),
                    &params,
                    KERNEL_MCP_PROTOCOL_VERSION,
                )
                .await
                {
                    Ok(ok) => {
                        let frame = build_ok_response_frame(&id, &ok.result_payload);
                        wr.write_all(&frame).await?;
                        wr.flush().await?;
                        break (ok.daemon_trust, ok.legacy_identity);
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
        daemon_trust,
        "mcp_server: connection initialized with daemon trust"
    );

    // Phase 2: post-initialize message pump. Any request after this
    // sees daemon trust; tools/call resolves identity from
    // `_meta.threadId` first, then temporarily falls back to the
    // initialize-time legacy token identity for older clients.
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
            Frame::Request {
                id,
                method,
                params,
                request_meta,
            } => {
                let resp = dispatch_request(
                    &method,
                    params,
                    request_meta,
                    &ctx,
                    daemon_trust,
                    legacy_identity.as_ref(),
                    &registry,
                )
                .await;
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
    method: &str,
    params: Value,
    request_meta: Option<Value>,
    ctx: &Arc<AppContext>,
    _daemon_trust: bool,
    legacy_identity: Option<&CardIdentity>,
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
            dispatch_tools_call(ctx, request_meta, params, legacy_identity, registry).await
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err(RpcError::method_not_found(other)),
    }
}

async fn dispatch_tools_call(
    ctx: &Arc<AppContext>,
    request_meta: Option<Value>,
    params: Value,
    legacy_identity: Option<&CardIdentity>,
    registry: &Arc<ToolRegistry>,
) -> Result<Value, RpcError> {
    let top_meta = request_meta_outcome(request_meta.as_ref());
    let params_meta = extract_request_meta_outcome(&params);
    for outcome in [&top_meta, &params_meta] {
        if matches!(outcome, MetaLookupOutcome::Malformed) {
            return Err(RpcError::invalid_params("_meta must be an object"));
        }
    }
    let thread_id = thread_id_from(&top_meta).or_else(|| thread_id_from(&params_meta));
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

    let thread_identity = resolve_thread_identity(ctx, thread_id, name).await;
    let identity = match thread_identity {
        Ok(identity) => identity,
        Err(thread_err) => match legacy_identity {
            Some(legacy) => ToolCallIdentity {
                card_id: legacy.card_id.as_str().to_string(),
                role: legacy.role,
                wave_id: legacy.wave_id.clone(),
                thread_id: "legacy-token-fallback".to_string(),
            },
            None => return Err(thread_err),
        },
    };

    let fut = handler(ctx.clone(), identity, arguments);
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

async fn resolve_thread_identity(
    ctx: &Arc<AppContext>,
    thread_id: Option<&str>,
    tool_name: &str,
) -> Result<ToolCallIdentity, RpcError> {
    let thread_id =
        thread_id.ok_or_else(|| RpcError::invalid_params("tools/call requires _meta.threadId"))?;
    let card_id =
        resolve_card_for_thread_runtime(ctx.repo.as_ref(), AgentProvider::Codex, thread_id)
            .await
            .map_err(|e| RpcError::internal(format!("tools/call thread lookup: {e}")))?
            .ok_or_else(|| {
                tracing::warn!(
                    target: "shared_codex_daemon::mcp_identity_miss",
                    thread_id,
                    tool = %tool_name,
                    "mcp_server: tools/call thread id did not resolve to a card"
                );
                RpcError::method_not_found(&format!("unknown thread_id: {thread_id}"))
            })?;
    let card = ctx
        .repo
        .card_get(&card_id)
        .await
        .map_err(|e| RpcError::internal(format!("tools/call card lookup: {e}")))?
        .ok_or_else(|| RpcError::method_not_found(&format!("unknown card_id: {card_id}")))?;
    let role = ctx
        .repo
        .card_role_get(&card_id)
        .await
        .map_err(|e| RpcError::internal(format!("tools/call card role lookup: {e}")))?
        .ok_or_else(|| RpcError::method_not_found(&format!("unknown card_id: {card_id}")))?;
    Ok(ToolCallIdentity {
        card_id,
        role,
        wave_id: Some(card.wave_id.to_string()),
        thread_id: thread_id.to_string(),
    })
}

#[derive(Clone, Copy)]
enum MetaLookupOutcome<'a> {
    Absent,
    Object(&'a Value),
    Malformed,
}

fn extract_request_meta_outcome(params: &Value) -> MetaLookupOutcome<'_> {
    request_meta_outcome(params.get("_meta"))
}

fn request_meta_outcome(meta: Option<&Value>) -> MetaLookupOutcome<'_> {
    match meta {
        None => MetaLookupOutcome::Absent,
        Some(v) if v.is_object() => MetaLookupOutcome::Object(v),
        Some(_) => MetaLookupOutcome::Malformed,
    }
}

fn thread_id_from<'a>(request_meta: &MetaLookupOutcome<'a>) -> Option<&'a str> {
    match request_meta {
        MetaLookupOutcome::Object(meta) => meta
            .get("threadId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        MetaLookupOutcome::Absent | MetaLookupOutcome::Malformed => None,
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
