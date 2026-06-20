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
//!      the token and establish a per-connection identity mode, then
//!      sends the response.
//!   3. After handshake, treats every subsequent `tools/call` as an
//!      invocation of a [`ToolRegistry`] handler, resolving identity
//!      according to the established connection identity.
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

use crate::db::{Repo, RouteRepo, SessionCardIdentity};
use crate::mcp_server::framing::{
    Frame, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};
use crate::mcp_server::handshake::{TOKEN_NOT_RECOGNIZED_CODE, handle_initialize};
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ConnectionIdentity, ToolCallIdentity, ToolDescriptor, ToolRegistry,
    require_role_any,
};
use crate::model::CardRole;
use crate::model::{new_id, now_ms};
use crate::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionPayload, ProbeSpec, SUPPORTED_FORGE_EVENT_KINDS,
};
use crate::operation::{OperationKey, OperationOutcome, OperationResult, OperationRuntime};
use crate::plugin_host::manifest::ToolKind;
use crate::session_projection_repo::AgentProvider;
use crate::state::WriteContext;
use calm_truth::wave_vcs_repo::SqlxWaveVcsRepo;
use calm_types::event::{ForgeEventSpec, ForgeMergeSubject};
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
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
const PLUGIN_TOOL_ROLES: &[CardRole] = &[CardRole::Spec, CardRole::Worker];

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
        repo: Arc<dyn Repo>,
        events: crate::event::EventBus,
        write: WriteContext,
        socket_path: PathBuf,
        shim_bin: PathBuf,
        registry: Arc<ToolRegistry>,
        daemon_token_hash: Option<String>,
        plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
        operation_runtime: Arc<tokio::sync::OnceCell<Arc<OperationRuntime>>>,
        gate_logs_dir: PathBuf,
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

        let wave_vcs = repo.sqlite_pool().map(SqlxWaveVcsRepo::shared);
        let route_repo: Arc<dyn RouteRepo> = repo;
        let ctx = Arc::new(AppContext {
            repo: route_repo,
            wave_vcs,
            events,
            write,
            daemon_token_hash,
            gate_logs_dir,
            plugin_host,
            operation_runtime,
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
    let connection_identity = loop {
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
                        break ok.connection_identity;
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

    let identity_mode = match &connection_identity {
        ConnectionIdentity::DaemonTrust => "daemon_trust",
        ConnectionIdentity::CardBound(_) => "card_bound",
    };
    tracing::info!(identity_mode, "mcp_server: connection initialized");

    // Phase 2: post-initialize message pump. Any request after this
    // resolves identity according to the explicit connection mode fixed
    // by initialize: daemon trust requires `_meta.threadId`, while a
    // card-bound connection may omit it and use the bound card.
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
                    &connection_identity,
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
    connection_identity: &ConnectionIdentity,
    registry: &Arc<ToolRegistry>,
) -> Result<Value, RpcError> {
    match method {
        "tools/list" => {
            // Resolve role per-call so shared-daemon connections (one socket,
            // many thread identities) get the right per-thread tools/list.
            // Card-bound sockets use their bound role when no threadId is
            // supplied; an explicit per-call threadId resolves independently.
            let top_meta = request_meta_outcome(request_meta.as_ref());
            let params_meta = extract_request_meta_outcome(&params);
            let thread_id = thread_id_from(&top_meta).or_else(|| thread_id_from(&params_meta));
            let descriptors = match connection_identity {
                ConnectionIdentity::DaemonTrust => match thread_id {
                    Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                        .await
                        .ok()
                    {
                        Some(identity) => {
                            let mut descriptors = registry.descriptors_for_role(identity.role);
                            extend_plugin_tool_descriptors_for_role(
                                ctx,
                                &mut descriptors,
                                identity.role,
                            )
                            .await;
                            descriptors
                        }
                        None => {
                            let mut descriptors =
                                registry.descriptors_visible_to_any_role(PLUGIN_TOOL_ROLES);
                            descriptors.extend(plugin_tool_descriptors(ctx).await);
                            descriptors
                        }
                    },
                    // Shared-daemon Codex sessions may send tools/list before
                    // a thread is attributed. Discovery can safely return the
                    // role-visible union because tools/call still resolves and
                    // enforces the exact per-thread identity.
                    None => {
                        let mut descriptors =
                            registry.descriptors_visible_to_any_role(PLUGIN_TOOL_ROLES);
                        descriptors.extend(plugin_tool_descriptors(ctx).await);
                        descriptors
                    }
                },
                ConnectionIdentity::CardBound(bound) => match thread_id {
                    Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                        .await
                        .ok()
                    {
                        Some(identity) if same_bound_session(&identity, bound) => {
                            let mut descriptors = registry.descriptors_for_role(identity.role);
                            extend_plugin_tool_descriptors_for_role(
                                ctx,
                                &mut descriptors,
                                identity.role,
                            )
                            .await;
                            descriptors
                        }
                        Some(identity) => {
                            warn_cross_session_reject(tid, &identity, bound);
                            Vec::new()
                        }
                        _ => Vec::new(),
                    },
                    None => {
                        ensure_card_bound_session_active(ctx, bound, "tools/list").await?;
                        let mut descriptors = registry.descriptors_for_role(bound.role);
                        extend_plugin_tool_descriptors_for_role(ctx, &mut descriptors, bound.role)
                            .await;
                        descriptors
                    }
                },
            };
            // Codex's `tools/list` expects `{ "tools": [...] }`. Each
            // entry is `{ name, description, inputSchema }`, optionally
            // carrying MCP `annotations` when a descriptor provides them.
            let tools: Vec<Value> = descriptors
                .into_iter()
                .map(|d| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("name".into(), Value::String(d.name));
                    obj.insert("description".into(), Value::String(d.description));
                    obj.insert("inputSchema".into(), d.input_schema);
                    if let Some(annotations) = d.annotations {
                        obj.insert("annotations".into(), annotations);
                    }
                    Value::Object(obj)
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            dispatch_tools_call(ctx, request_meta, params, connection_identity, registry).await
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err(RpcError::method_not_found(other)),
    }
}

async fn extend_plugin_tool_descriptors_for_role(
    ctx: &Arc<AppContext>,
    descriptors: &mut Vec<ToolDescriptor>,
    role: CardRole,
) {
    if PLUGIN_TOOL_ROLES.contains(&role) {
        descriptors.extend(plugin_tool_descriptors(ctx).await);
    }
}

async fn plugin_tool_descriptors(ctx: &Arc<AppContext>) -> Vec<ToolDescriptor> {
    let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
        return Vec::new();
    };

    let running_ids = plugin_host.running_plugin_ids().await;
    let mut descriptors = Vec::new();
    for manifest in plugin_host.registry().list() {
        let plugin_id = manifest.id;
        if !running_ids.contains(&plugin_id) {
            continue;
        }
        for entry in manifest.exposes_tools {
            descriptors.push(ToolDescriptor {
                // Plugin ids exclude `_` (is_valid_plugin_id), so `_` is an
                // unambiguous id↔tool boundary; tool names may contain `.`/`_`
                // after it.
                name: format!("plugin.{}_{}", plugin_id, entry.name),
                description: entry.description.unwrap_or_default(),
                input_schema: json!({ "type": "object" }),
                annotations: None,
                visible_to_roles: PLUGIN_TOOL_ROLES,
            });
        }
    }
    descriptors
}

async fn dispatch_tools_call(
    ctx: &Arc<AppContext>,
    request_meta: Option<Value>,
    params: Value,
    connection_identity: &ConnectionIdentity,
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

    if let Some(handler) = registry.lookup(name) {
        let identity =
            resolve_tools_call_identity(ctx, thread_id, name, connection_identity).await?;
        let fut = handler(ctx.clone(), identity, arguments);
        let raw = fut.await?;
        // Wrap the handler's raw payload in the MCP `CallToolResult`
        // envelope so codex's MCP client parses it. The kernel's
        // tools today return a JSON object; we surface it as a
        // single `text` content block + `structuredContent` field
        // so downstream agents can either parse the structured form
        // or read the text representation.
        let text = serde_json::to_string(&raw).unwrap_or_else(|_| "{}".to_string());
        return Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": raw,
            "isError": false,
        }));
    }

    dispatch_plugin_tools_call(ctx, thread_id, name, arguments, connection_identity).await
}

async fn resolve_tools_call_identity(
    ctx: &Arc<AppContext>,
    thread_id: Option<&str>,
    name: &str,
    connection_identity: &ConnectionIdentity,
) -> Result<ToolCallIdentity, RpcError> {
    match connection_identity {
        ConnectionIdentity::DaemonTrust => resolve_thread_identity(ctx, thread_id, name).await,
        ConnectionIdentity::CardBound(bound) => match thread_id {
            Some(tid) => {
                let identity = resolve_thread_identity(ctx, Some(tid), name).await?;
                if !same_bound_session(&identity, bound) {
                    warn_cross_session_reject(tid, &identity, bound);
                    return Err(cross_session_thread_error(tid, bound));
                }
                Ok(identity)
            }
            None => card_bound_tool_identity(ctx, bound).await,
        },
    }
}

async fn dispatch_plugin_tools_call(
    ctx: &Arc<AppContext>,
    thread_id: Option<&str>,
    name: &str,
    arguments: Value,
    connection_identity: &ConnectionIdentity,
) -> Result<Value, RpcError> {
    let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
        return Err(RpcError::method_not_found(&format!("tools/call: {name}")));
    };
    let running_ids = plugin_host.running_plugin_ids().await;
    let Some((plugin_id, tool_name, kind)) = plugin_tool_route(&plugin_host, name, &running_ids)?
    else {
        return Err(RpcError::method_not_found(&format!("tools/call: {name}")));
    };

    let identity = resolve_tools_call_identity(ctx, thread_id, name, connection_identity).await?;
    require_role_any(&identity, PLUGIN_TOOL_ROLES)?;
    let client = plugin_host
        .mcp_client(&plugin_id)
        .await
        .ok_or_else(|| RpcError::custom(-32002, format!("plugin `{plugin_id}` not running")))?;
    match kind {
        None => {
            let result = client.tools_call(&tool_name, arguments).await?;
            serde_json::to_value(result)
                .map_err(|e| RpcError::internal(format!("plugin tools/call serialization: {e}")))
        }
        Some(ToolKind::ForgeAction) => {
            dispatch_forge_action_plugin_tool(
                ctx, client, &plugin_id, &tool_name, arguments, identity,
            )
            .await
        }
    }
}

fn plugin_tool_route(
    plugin_host: &Arc<crate::plugin_host::PluginHost>,
    name: &str,
    running_ids: &BTreeSet<String>,
) -> Result<Option<(String, String, Option<ToolKind>)>, RpcError> {
    let Some(rest) = name.strip_prefix("plugin.") else {
        return Ok(None);
    };

    let mut candidates = Vec::new();
    for manifest in plugin_host.registry().list() {
        let plugin_id = manifest.id;
        if !running_ids.contains(&plugin_id) {
            continue;
        }
        let prefix = format!("{plugin_id}_");
        if let Some(tool_name) = rest.strip_prefix(&prefix)
            && let Some(entry) = manifest
                .exposes_tools
                .iter()
                .find(|entry| entry.name == tool_name)
        {
            candidates.push((plugin_id, tool_name.to_string(), entry.kind));
        }
    }

    match candidates.len() {
        0 => Ok(None),
        1 => {
            let (plugin_id, tool_name, kind) = candidates.remove(0);
            Ok(Some((plugin_id, tool_name, kind)))
        }
        _ => {
            // Unreachable by construction: plugin ids cannot contain `_`, so
            // the `_` id/tool boundary guarantees at most one running manifest
            // can match. Keep this as defense-in-depth against future changes.
            let mut matches = candidates
                .into_iter()
                .map(|(plugin_id, tool_name, _kind)| format!("plugin.{plugin_id}_{tool_name}"))
                .collect::<Vec<_>>();
            matches.sort();
            Err(RpcError::custom(
                RpcError::INVALID_PARAMS,
                format!(
                    "ambiguous plugin tool `{name}` matches {}",
                    matches.join(", ")
                ),
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct PluginForgePayload {
    argv: Vec<String>,
    idem_key: String,
    #[serde(default)]
    event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    subject: Option<ForgeMergeSubject>,
    #[serde(default)]
    context: serde_json::Map<String, Value>,
    #[serde(default)]
    probe: Option<ProbeSpec>,
    #[serde(default)]
    parked: bool,
}

#[derive(Serialize)]
struct SemanticForgePayload<'a> {
    idem_key: &'a str,
    argv: &'a [String],
    event_spec: Option<&'a ForgeEventSpec>,
    subject: Option<&'a ForgeMergeSubject>,
    context: &'a serde_json::Map<String, Value>,
}

async fn dispatch_forge_action_plugin_tool(
    ctx: &Arc<AppContext>,
    client: Arc<crate::plugin_host::McpClient>,
    plugin_id: &str,
    tool_name: &str,
    arguments: Value,
    identity: ToolCallIdentity,
) -> Result<Value, RpcError> {
    let result = client.tools_call(tool_name, arguments).await?;
    if result.is_error == Some(true) {
        return serde_json::to_value(result)
            .map_err(|e| RpcError::internal(format!("plugin tools/call serialization: {e}")));
    }

    let Some(structured) = result.structured_content else {
        return Err(malformed_forge_payload());
    };
    let payload: PluginForgePayload =
        serde_json::from_value(structured).map_err(|_| malformed_forge_payload())?;

    validate_plugin_forge_payload(&payload)?;
    if !trusted_forge_plugin(plugin_id) {
        return Err(RpcError::invalid_params(
            "plugin not trusted to submit forge actions",
        ));
    }

    let Some(runtime) = ctx.operation_runtime.get().cloned() else {
        return Err(RpcError::internal("operation runtime not bound"));
    };

    let wave_id = identity
        .wave_id
        .clone()
        .ok_or_else(|| RpcError::invalid_params("forge action requires a wave-scoped caller"))?;
    let cwd_lease = resolve_forge_cwd(ctx, &identity, &wave_id).await?;
    let result_path = forge_result_path(&payload.idem_key)?;
    let deadline_ms = now_ms() + forge_deadline_ms(payload.parked);

    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(payload.idem_key.clone()),
        payload_hash: semantic_payload_hash(&payload)?,
    };
    let parked = payload.parked;
    let forge_payload = ForgeActionPayload {
        wave_id,
        card_id: identity.card_id,
        subject: payload.subject,
        argv: payload.argv,
        idem_key: payload.idem_key,
        event_spec: payload.event_spec,
        context: payload.context,
        probe: payload.probe,
        cwd_lease,
        result_path,
        deadline_ms,
    };
    let operation_payload = serde_json::to_value(forge_payload)
        .map_err(|e| RpcError::internal(format!("forge-action payload serialization: {e}")))?;

    let op_id = match runtime
        .submit(FORGE_ACTION_KIND, key, operation_payload)
        .await
    {
        Ok(op_id) => op_id,
        Err(e) => return Ok(mcp_error_result(e.to_string())),
    };
    if parked {
        return Ok(mcp_success_result(json!({
            "op_id": op_id,
            "parked": true,
        })));
    }

    let outcome = match runtime.wait(&op_id).await {
        Ok(outcome) => outcome,
        Err(e) => return Ok(mcp_error_result(e.to_string())),
    };
    Ok(mcp_success_result(json!({
        "op_id": op_id,
        "parked": false,
        "result": operation_result_to_value(outcome),
    })))
}

fn validate_plugin_forge_payload(payload: &PluginForgePayload) -> Result<(), RpcError> {
    if payload.argv.is_empty() {
        return Err(malformed_forge_payload());
    }
    if payload.idem_key.trim().is_empty() {
        return Err(malformed_forge_payload());
    }
    if let Some(spec) = payload.event_spec.as_ref()
        && !SUPPORTED_FORGE_EVENT_KINDS.contains(&spec.event_kind.as_str())
    {
        return Err(RpcError::invalid_params(format!(
            "forge-action event_kind `{}` is not supported",
            spec.event_kind
        )));
    }
    Ok(())
}

async fn resolve_forge_cwd(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    wave_id: &str,
) -> Result<PathBuf, RpcError> {
    let wave = ctx
        .repo
        .wave_get(wave_id)
        .await
        .map_err(|e| RpcError::internal(format!("forge action wave lookup: {e}")))?
        .ok_or_else(|| RpcError::invalid_params(format!("unknown wave `{wave_id}`")))?;
    let wave_cwd = PathBuf::from(&wave.cwd);
    if !wave_cwd.is_absolute() {
        return Err(RpcError::invalid_params(
            "forge action requires an absolute wave cwd",
        ));
    }
    match identity.role {
        CardRole::Spec => Ok(wave_cwd),
        CardRole::Worker => {
            let lease = ctx
                .repo
                .workspace_lease_for_card(&identity.card_id)
                .await
                .map_err(|e| RpcError::internal(format!("workspace lease lookup: {e}")))?
                .ok_or_else(|| RpcError::invalid_params("no held workspace lease"))?;
            if lease.wave_id != wave_id {
                return Err(RpcError::invalid_params(
                    "workspace lease belongs to a different wave",
                ));
            }
            let lease_path = Path::new(&lease.path);
            if lease_path.is_absolute() {
                return Err(RpcError::invalid_params(
                    "workspace lease path must be relative",
                ));
            }
            Ok(wave_cwd.join(lease_path))
        }
        _ => Err(RpcError::invalid_params(
            "forge action requires a spec or worker caller",
        )),
    }
}

fn semantic_payload_hash(payload: &PluginForgePayload) -> Result<String, RpcError> {
    let semantic = SemanticForgePayload {
        idem_key: &payload.idem_key,
        argv: &payload.argv,
        event_spec: payload.event_spec.as_ref(),
        subject: payload.subject.as_ref(),
        context: &payload.context,
    };
    let bytes = serde_json::to_vec(&semantic)
        .map_err(|e| RpcError::internal(format!("forge-action hash serialization: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn forge_result_path(idem_key: &str) -> Result<PathBuf, RpcError> {
    let dir = forge_results_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        RpcError::internal(format!("create forge results dir {}: {e}", dir.display()))
    })?;
    Ok(dir
        .join(sanitize_forge_idem_key(idem_key))
        .with_extension("result"))
}

fn forge_results_dir() -> Result<PathBuf, RpcError> {
    let raw = std::env::var("NEIGE_FORGE_RESULTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("neige-forge-results"));
    if raw.is_absolute() {
        return Ok(raw);
    }
    let cwd = std::env::current_dir()
        .map_err(|e| RpcError::internal(format!("resolve current directory: {e}")))?;
    Ok(cwd.join(raw))
}

fn sanitize_forge_idem_key(idem_key: &str) -> String {
    let mut sanitized = idem_key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    sanitized.truncate(96);
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(idem_key.as_bytes());
        format!("idem-{:x}", hasher.finalize())
    } else {
        sanitized
    }
}

fn forge_deadline_ms(parked: bool) -> i64 {
    let default_secs = if parked { 900 } else { 300 };
    let secs = std::env::var("NEIGE_FORGE_DEADLINE_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default_secs);
    secs.saturating_mul(1000)
}

fn trusted_forge_plugin(plugin_id: &str) -> bool {
    let configured = std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
        .unwrap_or_else(|_| "dev.neige.git-forge".to_string());
    configured
        .split(',')
        .map(str::trim)
        .any(|trusted| trusted == plugin_id)
}

fn operation_result_to_value(result: OperationResult) -> Value {
    match result.outcome {
        OperationOutcome::Succeeded { result } => result,
        OperationOutcome::SucceededViaCollision {
            existing_op_id,
            result,
        } => json!({
            "outcome": "succeeded_via_collision",
            "existing_op_id": existing_op_id,
            "result": result,
        }),
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => json!({
            "outcome": "failed",
            "last_error": last_error,
            "from_phase": from_phase.as_str(),
            "last_error_class": last_error_class,
        }),
        OperationOutcome::Stuck { reason, from_phase } => json!({
            "outcome": "stuck",
            "reason": reason,
            "from_phase": from_phase.as_str(),
        }),
    }
}

fn malformed_forge_payload() -> RpcError {
    RpcError::invalid_params("forge-action plugin returned a malformed payload")
}

fn mcp_success_result(structured: Value) -> Value {
    let text = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false,
    })
}

fn mcp_error_result(message: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.clone() }],
        "structuredContent": { "error": message },
        "isError": true,
    })
}

async fn card_bound_tool_identity(
    ctx: &Arc<AppContext>,
    bound: &CardIdentity,
) -> Result<ToolCallIdentity, RpcError> {
    let card = ensure_card_bound_session_active(ctx, bound, "tools/call").await?;
    Ok(ToolCallIdentity {
        card_id: card.card_id.as_str().to_string(),
        role: card.role,
        session_id: bound.session_id.clone(),
        wave_id: Some(card.wave_id.as_str().to_string()),
        cove_id: card.cove_id.as_str().to_string(),
        thread_id: "card-bound".to_string(),
    })
}

async fn ensure_card_bound_session_active(
    ctx: &Arc<AppContext>,
    bound: &CardIdentity,
    method: &'static str,
) -> Result<SessionCardIdentity, RpcError> {
    let session_id = WorkerSessionId::from(bound.session_id.clone());
    let session = ctx
        .repo
        .session_get_by_id(&session_id)
        .await
        .map_err(|e| RpcError::internal(format!("{method} bound session lookup: {e}")))?
        .ok_or_else(|| {
            warn_bound_session_reject(method, bound, "missing worker session");
            bound_session_auth_error(method, bound)
        })?;
    if !session.state.is_active_authority() {
        warn_bound_session_reject(method, bound, session.state.as_db_str());
        return Err(bound_session_auth_error(method, bound));
    }

    let card = ctx
        .repo
        .card_identity_get_by_session(bound.session_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("{method} bound session card lookup: {e}")))?
        .ok_or_else(|| {
            warn_bound_session_reject(method, bound, "missing card session link");
            bound_session_auth_error(method, bound)
        })?;
    if card.card_id.as_str() != bound.card_id.as_str()
        || card.wave_id != session.wave_id
        || card.cove_id.as_str() != bound.cove_id.as_str()
    {
        warn_bound_session_reject(method, bound, "card session link drift");
        return Err(bound_session_auth_error(method, bound));
    }
    Ok(card)
}

fn warn_bound_session_reject(method: &str, bound: &CardIdentity, reason: &str) {
    tracing::warn!(
        target: "mcp_server::bound_session_reject",
        method,
        bound_card_id = %bound.card_id.as_str(),
        bound_session_id = %bound.session_id,
        reason,
        "mcp_server: card-bound session rejected"
    );
}

fn bound_session_auth_error(method: &str, bound: &CardIdentity) -> RpcError {
    RpcError::custom(
        TOKEN_NOT_RECOGNIZED_CODE,
        format!(
            "{method}: bound session `{}` did not resolve to an active session",
            bound.session_id
        ),
    )
}

fn same_bound_session(identity: &ToolCallIdentity, bound: &CardIdentity) -> bool {
    identity.session_id.as_str() == bound.session_id.as_str()
}

fn warn_cross_session_reject(thread_id: &str, identity: &ToolCallIdentity, bound: &CardIdentity) {
    let resolved_card_id = identity.card_id.as_str();
    let bound_card_id = bound.card_id.as_str();
    let resolved_session_id = identity.session_id.as_str();
    let bound_session_id = bound.session_id.as_str();
    tracing::warn!(
        target: "mcp_server::cross_session_reject",
        thread_id = %thread_id,
        resolved_card_id = %resolved_card_id,
        bound_card_id = %bound_card_id,
        resolved_session_id = %resolved_session_id,
        bound_session_id = %bound_session_id,
        "mcp_server: cross-session _meta.threadId rejected"
    );
}

fn cross_session_thread_error(thread_id: &str, bound: &CardIdentity) -> RpcError {
    RpcError::invalid_params(format!(
        "tools/call: _meta.threadId `{thread_id}` resolves to a session other than this connection's bound session `{}`",
        bound.session_id
    ))
}

async fn resolve_thread_identity(
    ctx: &Arc<AppContext>,
    thread_id: Option<&str>,
    tool_name: &str,
) -> Result<ToolCallIdentity, RpcError> {
    let thread_id =
        thread_id.ok_or_else(|| RpcError::invalid_params("tools/call requires _meta.threadId"))?;
    let runtime = ctx
        .repo
        .session_projection_active_by_thread(AgentProvider::Codex, thread_id)
        .await
        .map_err(|e| RpcError::internal(format!("tools/call thread lookup: {e}")))?
        .ok_or_else(|| {
            tracing::warn!(
                target: "shared_codex_daemon::mcp_identity_miss",
                thread_id,
                tool = %tool_name,
                "mcp_server: tools/call thread id did not resolve to a session"
            );
            RpcError::method_not_found(&format!("unknown thread_id: {thread_id}"))
        })?;
    let card = ctx
        .repo
        .card_identity_get_by_session(&runtime.id)
        .await
        .map_err(|e| RpcError::internal(format!("tools/call session card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::method_not_found(&format!("unknown session_id: {}", runtime.id))
        })?;
    if card.card_id.as_str() != runtime.card_id {
        return Err(RpcError::method_not_found(&format!(
            "unknown session_id: {}",
            runtime.id
        )));
    }
    Ok(ToolCallIdentity {
        card_id: card.card_id.as_str().to_string(),
        role: card.role,
        session_id: runtime.id.clone(),
        wave_id: Some(card.wave_id.as_str().to_string()),
        cove_id: card.cove_id.as_str().to_string(),
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
/// PR7a (#136) — shared helper for production boot and integration tests.
pub(crate) fn default_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("mcp").join("kernel.sock")
}
