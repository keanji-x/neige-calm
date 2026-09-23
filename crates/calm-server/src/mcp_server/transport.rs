//! UDS listener + per-connection JSON-RPC pump for the kernel-as-MCP-server.
//! One socket under `<data_dir>/mcp/kernel.sock` (mode 0600); a connection must `initialize` before any `tools/*` request.

pub(crate) mod worker_grants;
pub(crate) use worker_grants::resolve_dispatch_plugin_tools;

use crate::db::{Repo, SessionCardIdentity};
use crate::forge_trust::trusted_forge_plugin;
use crate::mcp_server::framing::{
    Frame, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};
use crate::mcp_server::handshake::{TOKEN_NOT_RECOGNIZED_CODE, handle_initialize};
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ConnectionIdentity, ToolCallIdentity, ToolDescriptor, ToolRegistry,
    require_role_any,
};
use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
use crate::model::CardRole;
use crate::model::{new_id, now_ms};
use crate::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionPayload, ProbeSpec, SUPPORTED_FORGE_EVENT_KINDS,
};
use crate::operation::{OperationKey, OperationOutcome, OperationResult, OperationRuntime};
use crate::plugin_host::ConnectorClient;
use crate::plugin_host::manifest::ToolKind;
use crate::session_projection_repo::AgentProvider;
use crate::state::WriteContext;
use calm_types::event::{ForgeEventSpec, ForgeMergeSubject};
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

/// Protocol version advertised in `initialize`; the request's version is not validated.
pub const KERNEL_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// The per-card token is the credential; the socket's filesystem ACL is the perimeter (same uid only).
const SOCKET_MODE: u32 = 0o600;

/// Bounds a pathological stalled connect; a timeout falls through to the stale-file reclaim path, same as `ECONNREFUSED`.
const LIVE_LISTENER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
const PLUGIN_TOOL_ROLES: &[CardRole] = &[CardRole::Planner, CardRole::Worker];

#[derive(Clone, Debug)]
pub struct McpShimConfig {
    /// Path to the `neige-mcp-stdio-shim` binary, resolved at boot.
    pub shim_bin: PathBuf,
    /// UDS the shim should `connect()` to.
    pub socket_path: PathBuf,
}

pub struct McpServer {
    pub terminal_interaction:
        Arc<tokio::sync::OnceCell<Arc<crate::terminal_interaction::TerminalInteraction>>>,
    pub shim_config: McpShimConfig,
    #[allow(dead_code)]
    listener_task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl McpServer {
    #[cfg(test)]
    pub(crate) fn new_for_test(shim_config: McpShimConfig) -> Arc<Self> {
        Arc::new(Self {
            terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
            shim_config,
            listener_task: std::sync::Mutex::new(None),
        })
    }

    /// If `socket_path` already exists, probe it: a live peer means another process serves the same path and we refuse to boot rather than
    /// unlink-and-rebind (the unlink would steal the path without breaking its socket); a connect failure is the stale-file case, unlinked and rebound.
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
        task_budget_default: i64,
    ) -> anyhow::Result<Arc<Self>> {
        let ctx = AppContext::new(
            repo,
            events,
            write,
            daemon_token_hash,
            plugin_host,
            operation_runtime,
            gate_logs_dir,
            task_budget_default,
            Arc::new(crate::preview::PreviewRegistry::disabled()),
        );
        Self::spawn_with_context(ctx, socket_path, shim_bin, registry).await
    }

    /// [`Self::spawn`] with a context the caller built and keeps, so the HTTP layer and MCP resolve through one `SeriesResolver`.
    pub async fn spawn_with_context(
        ctx: Arc<AppContext>,
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
        if socket_path.exists() {
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
                    let _ = std::fs::remove_file(&socket_path);
                }
            }
        }

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| anyhow::anyhow!("bind mcp socket {}: {e}", socket_path.display()))?;

        // The default umask leaves a world-readable socket.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(SOCKET_MODE);
            std::fs::set_permissions(&socket_path, perms)
                .map_err(|e| anyhow::anyhow!("chmod mcp socket {}: {e}", socket_path.display()))?;
        }

        let terminal_interaction = ctx.terminal_interaction.clone();
        let socket_for_handle = socket_path.clone();
        let task = tokio::spawn(accept_loop(listener, ctx, registry, socket_for_handle));

        tracing::info!(
            socket = %socket_path.display(),
            "mcp_server: kernel-as-MCP-server listening"
        );

        Ok(Arc::new(Self {
            terminal_interaction,
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

async fn handle_connection(
    stream: UnixStream,
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
) -> anyhow::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut line = String::new();

    // Phase 1: no identity is bound until `initialize` succeeds; every other request before then is rejected.
    let connection_identity = loop {
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
                        return Ok(());
                    }
                }
            }
            Frame::Request { id, method, .. } => {
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
                // A response arriving pre-handshake is wrong-direction noise.
            }
        }
    };

    let identity_mode = match &connection_identity {
        ConnectionIdentity::DaemonTrust => "daemon_trust",
        ConnectionIdentity::CardBound(_) => "card_bound",
    };
    tracing::info!(identity_mode, "mcp_server: connection initialized");

    // Phase 2: daemon trust requires `_meta.threadId`, while a card-bound connection may omit it and use the bound card.
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
                tracing::debug!(method = %method, "mcp_server: notification dropped (PR7a no-op)");
            }
            Frame::Response { id, .. } => {
                tracing::debug!(?id, "mcp_server: stray response from client (ignored)");
            }
        }
    }
}

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
            // Resolve role per-call so shared-daemon connections (one socket, many thread identities) get the right per-thread tools/list.
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
                            // Plugin tools are scoped to the resolved thread's track.
                            let scope =
                                plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
                            let mut descriptors = registry.descriptors_for_role(identity.role);
                            extend_plugin_tool_descriptors_for_role(
                                ctx,
                                &mut descriptors,
                                &identity,
                                &scope,
                            )
                            .await?;
                            descriptors
                        }
                        None => {
                            // Unresolvable threadId: no track context, so the scope is the union ("discovery wide, dispatch strict"); tools/call still enforces per-thread identity and per-track scope.
                            let scope = plugin_scope_for_track(ctx, None).await;
                            let mut descriptors =
                                registry.descriptors_visible_to_any_role(PLUGIN_TOOL_ROLES);
                            descriptors.extend(plugin_tool_descriptors(ctx, &scope).await);
                            descriptors
                        }
                    },
                    // Shared-daemon Codex sessions may send tools/list before a thread is attributed. Discovery returns the role-visible union because tools/call still enforces identity and scope; the residual exposure is tool names only.
                    None => {
                        let scope = plugin_scope_for_track(ctx, None).await;
                        let mut descriptors =
                            registry.descriptors_visible_to_any_role(PLUGIN_TOOL_ROLES);
                        descriptors.extend(plugin_tool_descriptors(ctx, &scope).await);
                        descriptors
                    }
                },
                ConnectionIdentity::CardBound(bound) => match thread_id {
                    Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                        .await
                        .ok()
                    {
                        Some(identity) if same_bound_session(&identity, bound) => {
                            let scope =
                                plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
                            let mut descriptors = registry.descriptors_for_role(identity.role);
                            extend_plugin_tool_descriptors_for_role(
                                ctx,
                                &mut descriptors,
                                &identity,
                                &scope,
                            )
                            .await?;
                            descriptors
                        }
                        Some(identity) => {
                            warn_cross_session_reject(tid, &identity, bound);
                            Vec::new()
                        }
                        _ => Vec::new(),
                    },
                    None => {
                        let card =
                            ensure_card_bound_session_active(ctx, bound, "tools/list").await?;
                        let scope = plugin_scope_for_track(ctx, Some(card.track_id.as_str())).await;
                        let mut descriptors = registry.descriptors_for_role(bound.role);
                        let identity = card_bound_tool_identity(ctx, bound).await?;
                        extend_plugin_tool_descriptors_for_role(
                            ctx,
                            &mut descriptors,
                            &identity,
                            &scope,
                        )
                        .await?;
                        descriptors
                    }
                },
            };
            // Codex's `tools/list` expects `{ "tools": [...] }`.
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
    identity: &ToolCallIdentity,
    scope: &TrackPluginScope,
) -> Result<(), RpcError> {
    if PLUGIN_TOOL_ROLES.contains(&identity.role) {
        descriptors.extend(plugin_tool_descriptors(ctx, scope).await);
    }
    worker_grants::filter(ctx, identity, descriptors).await
}

/// Plugin tool descriptors visible under `scope`; kernel `calm.*` descriptors never route through here.
async fn plugin_tool_descriptors(
    ctx: &Arc<AppContext>,
    scope: &TrackPluginScope,
) -> Vec<ToolDescriptor> {
    let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
        return Vec::new();
    };

    let running_ids = plugin_host.running_plugin_ids().await;
    plugin_tool_descriptors_from(plugin_host.registry().list(), &running_ids, scope)
}

/// The only place `plugin.<id>_<tool>` names are minted for discovery; `plugin_tool_route` is its inverse.
fn plugin_tool_descriptors_from(
    manifests: Vec<crate::plugin_host::Manifest>,
    running_ids: &BTreeSet<String>,
    scope: &TrackPluginScope,
) -> Vec<ToolDescriptor> {
    let mut descriptors = Vec::new();
    for manifest in manifests {
        let plugin_id = manifest.id;
        if !running_ids.contains(&plugin_id) || !scope.allows(&plugin_id) {
            continue;
        }
        for entry in manifest.exposes_tools {
            descriptors.push(ToolDescriptor {
                // Plugin ids exclude `_`, so `_` is an unambiguous id↔tool boundary.
                name: format!("plugin.{}_{}", plugin_id, entry.name),
                description: entry.description.unwrap_or_default(),
                input_schema: entry
                    .input_schema
                    .unwrap_or_else(|| json!({ "type": "object" })),
                annotations: entry.annotations,
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
        worker_grants::require(ctx, &identity, name).await?;
        let fut = handler(ctx.clone(), identity, arguments);
        // Serialize the typed envelope once; native images must not be converted into text by wrapping again.
        return Ok(json!(fut.await?));
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
    // Identity FIRST, before any route knowledge, so identity failures are uniform whether or not `name` exists.
    let identity = resolve_tools_call_identity(ctx, thread_id, name, connection_identity).await?;

    // One construction for EVERY existence-shaped rejection below so the error object is byte-identical and cannot be an existence oracle.
    let unknown_tool = || RpcError::method_not_found(&format!("tools/call: {name}"));

    let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
        return Err(unknown_tool());
    };
    let running_ids = plugin_host.running_plugin_ids().await;
    let Some((plugin_id, tool_name, kind)) =
        plugin_tool_route(plugin_host.registry(), name, &running_ids)?
    else {
        return Err(unknown_tool());
    };

    // A track bound to a template may only call the owning plugin's tools; rejected via `unknown_tool` so a bound track cannot probe for other plugins' tools.
    if !plugin_scope_for_track(ctx, identity.track_id.as_deref())
        .await
        .allows(&plugin_id)
    {
        return Err(unknown_tool());
    }
    require_role_any(&identity, PLUGIN_TOOL_ROLES)?;
    worker_grants::require(ctx, &identity, name).await?;
    match kind {
        None => {
            // Connector tools materialize with `kind: None`, so without this arm they would fall through to the stdio-only accessor and get a spurious `-32002 not running`.
            let client = plugin_host
                .connector_client(&plugin_id)
                .await
                .ok_or_else(|| {
                    RpcError::custom(-32002, format!("plugin `{plugin_id}` not running"))
                })?;
            // Only a Planner's call carrying a track is recorded for `calm.source.capture`; the identity is the resolved one, never anything in the request.
            let record_for = match (&identity.role, identity.track_id.as_deref()) {
                (CardRole::Planner, Some(track_id)) => {
                    Some((track_id.to_string(), arguments.clone()))
                }
                _ => None,
            };
            let called = match &client {
                // The Track rides along only to LOCAL plugins; a remote connector is a third party that must not learn which Track a reader is looking at.
                ConnectorClient::Stdio(c) => {
                    c.tools_call(&tool_name, arguments, identity.track_id.as_deref())
                        .await
                }
                ConnectorClient::Http(c) => c.tools_call(&tool_name, arguments).await,
                // An `Ok` result carries the child's own `isError` verdict, an `Err` is a kernel-side refusal.
                ConnectorClient::Cli(c) => c.tools_call(&tool_name, arguments).await,
            };
            // A call that produced no result (transport error, unparseable reply, disconnect) still replaces the key's entry with `Error`, so the previous body is not capturable any more.
            if let Some((track_id, args)) = record_for {
                match &called {
                    Ok(result) => ctx
                        .plugin_results
                        .record(&track_id, &plugin_id, &tool_name, &args, result),
                    Err(_) => ctx
                        .plugin_results
                        .record_failure(&track_id, &plugin_id, &tool_name, &args),
                }
            }
            let result = called?;
            serde_json::to_value(result)
                .map_err(|e| RpcError::internal(format!("plugin tools/call serialization: {e}")))
        }
        Some(ToolKind::ForgeAction) => {
            if !trusted_forge_plugin(&plugin_id) {
                return Err(RpcError::invalid_params(
                    "plugin not trusted to submit forge actions",
                ));
            }
            // Forge actions are stdio-only; a connector's materialized tools always carry `kind: None`.
            let client = plugin_host.mcp_client(&plugin_id).await.ok_or_else(|| {
                RpcError::custom(-32002, format!("plugin `{plugin_id}` not running"))
            })?;
            dispatch_forge_action_plugin_tool(
                ctx, client, &plugin_id, &tool_name, arguments, identity,
            )
            .await
        }
    }
}

/// Inverse of [`plugin_tool_descriptors_from`]: resolve a minted `plugin.<id>_<tool>` name back to its owner.
fn plugin_tool_route(
    registry: &crate::plugin_host::PluginRegistry,
    name: &str,
    running_ids: &BTreeSet<String>,
) -> Result<Option<(String, String, Option<ToolKind>)>, RpcError> {
    let Some(rest) = name.strip_prefix("plugin.") else {
        return Ok(None);
    };

    let mut candidates = Vec::new();
    for manifest in registry.list() {
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
            // Unreachable by construction (plugin ids cannot contain `_`); kept as defense-in-depth.
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

/// Four outcomes, not a boolean: the three negatives become distinct `pending` reasons and all mean "do not call, do not store".
#[derive(Debug, Clone)]
pub(crate) enum ToolEntry {
    /// `registry.get(plugin_id)` is `None`.
    NotInstalled,
    /// Installed, exposes the tool, but not in the running set.
    NotRunning,
    /// Installed, but `exposes_tools` has no entry of that name.
    NotExposed,
    Found(crate::plugin_host::manifest::ExposedTool),
}

impl ToolEntry {
    /// The `pending` reason for a negative outcome; `None` for `Found`.
    pub(crate) fn miss_reason(&self, plugin_id: &str, tool: &str) -> Option<String> {
        match self {
            Self::NotInstalled => Some(format!("plugin {plugin_id} is not installed")),
            Self::NotRunning => Some(format!("plugin {plugin_id} is not running")),
            Self::NotExposed => Some(format!("plugin {plugin_id} does not expose {tool}")),
            Self::Found(_) => None,
        }
    }
}

/// Exact `(plugin_id, tool)` lookup — never a `plugin.{id}_{tool}` string re-parse, which would land `plugin.aa_b_c` on plugin `aa`'s tool `b_c`.
pub(crate) fn plugin_tool_entry(
    registry: &crate::plugin_host::PluginRegistry,
    running_ids: &BTreeSet<String>,
    plugin_id: &str,
    tool: &str,
) -> ToolEntry {
    let Some(manifest) = registry.get(plugin_id) else {
        return ToolEntry::NotInstalled;
    };
    let Some(entry) = manifest
        .exposes_tools
        .iter()
        .find(|entry| entry.name == tool)
    else {
        return ToolEntry::NotExposed;
    };
    if !running_ids.contains(plugin_id) {
        return ToolEntry::NotRunning;
    }
    ToolEntry::Found(entry.clone())
}

#[derive(Debug, Deserialize)]
pub(crate) struct PluginForgePayload {
    pub(crate) argv: Vec<String>,
    pub(crate) idem_key: String,
    #[serde(default)]
    pub(crate) event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    pub(crate) subject: Option<ForgeMergeSubject>,
    #[serde(default)]
    pub(crate) context: serde_json::Map<String, Value>,
    #[serde(default)]
    pub(crate) probe: Option<ProbeSpec>,
    #[serde(default)]
    pub(crate) parked: bool,
}

/// Semantic subset used for idempotency payload comparison. `argv` is excluded so a retry with edited volatile argv dedups instead of conflicting;
/// changing this field set needs a boot-time recompute migration for stored forge-action `payload_hash` values.
#[derive(Serialize)]
struct SemanticForgePayload<'a> {
    idem_key: &'a str,
    event_spec: Option<&'a ForgeEventSpec>,
    subject: Option<&'a ForgeMergeSubject>,
    context: &'a serde_json::Map<String, Value>,
    probe: Option<&'a ProbeSpec>,
}

pub(crate) struct ForgeActionSubmission {
    pub(crate) op_id: String,
    pub(crate) parked: bool,
}

pub(crate) async fn submit_forge_action(
    ctx: &Arc<AppContext>,
    plugin_id: &str,
    track_id: String,
    card_id: String,
    cwd_lease: PathBuf,
    payload: PluginForgePayload,
) -> Result<std::result::Result<ForgeActionSubmission, String>, RpcError> {
    // A malformed payload is answered before the runtime is consulted (the pre-refactor order).
    validate_plugin_forge_payload(&payload)?;

    let Some(runtime) = ctx.operation_runtime.get().cloned() else {
        return Err(RpcError::internal("operation runtime not bound"));
    };
    submit_forge_action_with_key(
        &runtime,
        &ctx.gate_logs_dir,
        plugin_id,
        track_id,
        card_id,
        cwd_lease,
        payload,
        new_id(),
    )
    .await
}

/// `submit_forge_action` with the caller's `operation_key`: a kernel delivery re-submits its persisted key.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn submit_forge_action_with_key(
    runtime: &Arc<OperationRuntime>,
    gate_logs_dir: &Path,
    plugin_id: &str,
    track_id: String,
    card_id: String,
    cwd_lease: PathBuf,
    payload: PluginForgePayload,
    operation_key: String,
) -> Result<std::result::Result<ForgeActionSubmission, String>, RpcError> {
    validate_plugin_forge_payload(&payload)?;

    let parked = payload.parked;
    let idempotency_key = format!("{plugin_id}:{track_id}:{card_id}:{}", payload.idem_key);
    let result_path = forge_result_path(gate_logs_dir, &idempotency_key)?;
    let deadline_ms = now_ms() + forge_deadline_ms(payload.parked);

    let key = OperationKey {
        operation_key,
        idempotency_key: Some(idempotency_key),
        payload_hash: semantic_payload_hash(&payload)?,
    };
    let forge_payload = ForgeActionPayload {
        track_id,
        card_id,
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

    match runtime
        .submit(FORGE_ACTION_KIND, key, operation_payload)
        .await
    {
        Ok(op_id) => Ok(Ok(ForgeActionSubmission { op_id, parked })),
        Err(e) => Ok(Err(e.to_string())),
    }
}

async fn dispatch_forge_action_plugin_tool(
    ctx: &Arc<AppContext>,
    client: Arc<crate::plugin_host::McpClient>,
    plugin_id: &str,
    tool_name: &str,
    arguments: Value,
    identity: ToolCallIdentity,
) -> Result<Value, RpcError> {
    let result = client
        .tools_call(tool_name, arguments, identity.track_id.as_deref())
        .await?;
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

    let track_id = identity
        .track_id
        .clone()
        .ok_or_else(|| RpcError::invalid_params("forge action requires a track-scoped caller"))?;
    let card_id = identity.card_id.clone();
    let cwd_lease = resolve_forge_cwd(ctx, &identity, &track_id).await?;

    let submitted =
        match submit_forge_action(ctx, plugin_id, track_id, card_id, cwd_lease, payload).await? {
            Ok(submitted) => submitted,
            Err(e) => return Ok(mcp_error_result(e)),
        };
    let Some(runtime) = ctx.operation_runtime.get().cloned() else {
        return Err(RpcError::internal("operation runtime not bound"));
    };
    if submitted.parked {
        let result = match runtime.operation_result(&submitted.op_id).await {
            Ok(result) => result,
            Err(e) => return Ok(mcp_error_result(e.to_string())),
        };
        if let Some(result) = result {
            return Ok(operation_result_to_mcp_result(result));
        }
        return Ok(mcp_success_result(json!({
            "op_id": submitted.op_id,
            "parked": true,
        })));
    }

    let outcome = match runtime.wait(&submitted.op_id).await {
        Ok(outcome) => outcome,
        Err(e) => return Ok(mcp_error_result(e.to_string())),
    };
    Ok(operation_result_to_mcp_result(outcome))
}

fn validate_plugin_forge_payload(payload: &PluginForgePayload) -> Result<(), RpcError> {
    if payload.argv.is_empty() {
        return Err(malformed_forge_payload());
    }
    if payload.idem_key.trim().is_empty() {
        return Err(malformed_forge_payload());
    }
    if let Some(event_spec) = payload.event_spec.as_ref()
        && !SUPPORTED_FORGE_EVENT_KINDS.contains(&event_spec.event_kind.as_str())
    {
        return Err(RpcError::invalid_params(format!(
            "forge-action event_kind `{}` is not supported",
            event_spec.event_kind
        )));
    }
    Ok(())
}

async fn resolve_forge_cwd(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    track_id: &str,
) -> Result<PathBuf, RpcError> {
    let track = ctx
        .repo
        .track_get(track_id)
        .await
        .map_err(|e| RpcError::internal(format!("forge action track lookup: {e}")))?
        .ok_or_else(|| RpcError::invalid_params(format!("unknown track `{track_id}`")))?;
    if track.area_id.as_str() != identity.area_id.as_str() {
        return Err(RpcError::invalid_params(
            "forge action track belongs to a different area",
        ));
    }
    let track_cwd = PathBuf::from(&track.workspace.path);
    if !track_cwd.is_absolute() {
        return Err(RpcError::invalid_params(
            "forge action requires an absolute track cwd",
        ));
    }
    match identity.role {
        CardRole::Planner => Ok(track_cwd),
        CardRole::Worker => {
            let lease = ctx
                .repo
                .workspace_lease_for_card(&identity.card_id)
                .await
                .map_err(|e| RpcError::internal(format!("workspace lease lookup: {e}")))?
                .ok_or_else(|| RpcError::invalid_params("no held workspace lease"))?;
            if lease.track_id != track_id {
                return Err(RpcError::invalid_params(
                    "workspace lease belongs to a different track",
                ));
            }
            let lease_path = Path::new(&lease.path);
            if !lease_path.is_absolute()
                && !lease_path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
            {
                return Err(RpcError::invalid_params(
                    "workspace lease path contains invalid relative segments",
                ));
            }
            if lease_path.as_os_str().is_empty() {
                return Err(RpcError::invalid_params(
                    "workspace lease path must not be empty",
                ));
            }
            if lease_path.is_absolute() {
                Ok(lease_path.to_path_buf())
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|e| RpcError::internal(format!("resolve current directory: {e}")))?;
                Ok(cwd.join(lease_path))
            }
        }
        _ => Err(RpcError::invalid_params(
            "forge action requires a planner or worker caller",
        )),
    }
}

pub(crate) fn semantic_payload_hash(payload: &PluginForgePayload) -> Result<String, RpcError> {
    let semantic = SemanticForgePayload {
        idem_key: &payload.idem_key,
        event_spec: payload.event_spec.as_ref(),
        subject: payload.subject.as_ref(),
        context: &payload.context,
        probe: payload.probe.as_ref(),
    };
    let bytes = serde_json::to_vec(&semantic)
        .map_err(|e| RpcError::internal(format!("forge-action hash serialization: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn forge_result_path(gate_logs_dir: &Path, idem_key: &str) -> Result<PathBuf, RpcError> {
    let dir = forge_results_dir(gate_logs_dir)?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        RpcError::internal(format!("create forge results dir {}: {e}", dir.display()))
    })?;
    Ok(dir.join(forge_result_filename(idem_key)))
}

fn forge_results_dir(gate_logs_dir: &Path) -> Result<PathBuf, RpcError> {
    let raw = std::env::var("NEIGE_FORGE_RESULTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_forge_results_dir(gate_logs_dir));
    if raw.is_absolute() {
        return Ok(raw);
    }
    let cwd = std::env::current_dir()
        .map_err(|e| RpcError::internal(format!("resolve current directory: {e}")))?;
    Ok(cwd.join(raw))
}

fn default_forge_results_dir(gate_logs_dir: &Path) -> PathBuf {
    gate_logs_dir
        .parent()
        .map(|parent| parent.join("forge-results"))
        .unwrap_or_else(|| gate_logs_dir.join("forge-results"))
}

fn forge_result_filename(idem_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(idem_key.as_bytes());
    format!("{:x}.result", hasher.finalize())
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

fn operation_result_to_mcp_result(result: OperationResult) -> Value {
    let op_id = result.op_id;
    match result.outcome {
        OperationOutcome::Succeeded { result } => mcp_success_result(json!({
            "op_id": op_id,
            "parked": false,
            "result": result,
        })),
        OperationOutcome::SucceededViaCollision {
            existing_op_id,
            result,
        } => mcp_success_result(json!({
            "op_id": op_id,
            "parked": false,
            "result": {
                "outcome": "succeeded_via_collision",
                "existing_op_id": existing_op_id,
                "result": result,
            },
        })),
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => {
            let message = format!("forge action operation {op_id} failed: {last_error}");
            mcp_error_result_with_structured(
                message.clone(),
                json!({
                    "error": message,
                    "op_id": op_id,
                    "parked": false,
                    "outcome": "failed",
                    "last_error": last_error,
                    "from_phase": from_phase.as_str(),
                    "last_error_class": last_error_class,
                }),
            )
        }
        OperationOutcome::Stuck { reason, from_phase } => {
            let message = format!("forge action operation {op_id} stuck: {reason}");
            mcp_error_result_with_structured(
                message.clone(),
                json!({
                    "error": message,
                    "op_id": op_id,
                    "parked": false,
                    "outcome": "stuck",
                    "reason": reason,
                    "from_phase": from_phase.as_str(),
                }),
            )
        }
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
    mcp_error_result_with_structured(message.clone(), json!({ "error": message }))
}

fn mcp_error_result_with_structured(message: String, structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.clone() }],
        "structuredContent": structured,
        "isError": true,
    })
}

/// Discovery and routing for connector-materialized tools, against a registry in the state `spawn_admitted` leaves it in.
#[cfg(test)]
mod connector_tool_routing_tests {
    use super::*;
    use crate::plugin_host::{Manifest, PluginRegistry};

    const CONNECTOR_ID: &str = "mcp-wisburg";
    /// Underscores, not hyphens: the tool name must contain `_`, the character the `plugin.<id>_<tool>` boundary is built on.
    const UNDERSCORE_TOOL: &str = "list_institutional_reports";
    const OTHER_TOOL: &str = "get_report_detail";
    const DENIED_TOOL: &str = "admin_purge_everything";

    fn connector_manifest(id: &str, tools: &[&str]) -> Manifest {
        let manifest = json!({
            "manifest_version": 1,
            "kind": "mcp-http",
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Wisburg",
            "mcp_http": {
                "url": "https://mcp.example.com/mcp",
                "tools_allow": tools,
            },
        });
        Manifest::parse(&manifest.to_string()).expect("connector manifest parses")
    }

    /// One connector in the post-materialization state: the tools `spawn_admitted` would have materialized folded into `exposes_tools`.
    fn materialized_connector(id: &str, allow: &[&str], served: &[&str]) -> Manifest {
        let upstream: Vec<Value> = served
            .iter()
            .map(|name| json!({ "name": name, "inputSchema": { "type": "object" } }))
            .collect();
        materialized_connector_from_upstream(id, allow, &upstream)
    }

    /// Same, but the upstream `tools/list` entries carry NO `inputSchema` (`input_schema: None`).
    fn materialized_connector_schemaless(id: &str, allow: &[&str], served: &[&str]) -> Manifest {
        let upstream: Vec<Value> = served.iter().map(|name| json!({ "name": name })).collect();
        materialized_connector_from_upstream(id, allow, &upstream)
    }

    fn materialized_connector_from_upstream(
        id: &str,
        allow: &[&str],
        upstream: &[Value],
    ) -> Manifest {
        let mut manifest = connector_manifest(id, allow);
        let block = manifest.mcp_http.clone().expect("mcp_http block");
        manifest.exposes_tools =
            crate::plugin_host::connector::materialize_http_tools(id, &block, upstream);
        manifest
    }

    /// Registry seeded with every connector in one build-time pass.
    fn registry_after_materialization(connectors: &[(&str, &[&str], &[&str])]) -> PluginRegistry {
        PluginRegistry::from_manifests(
            connectors
                .iter()
                .map(|(id, allow, served)| (materialized_connector(id, allow, served), None)),
        )
    }

    fn running(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unbound_track_sees_allowlisted_connector_tools_and_nothing_else() {
        let registry = registry_after_materialization(&[(
            CONNECTOR_ID,
            &[UNDERSCORE_TOOL, OTHER_TOOL],
            &[UNDERSCORE_TOOL, OTHER_TOOL, DENIED_TOOL],
        )]);
        // `TrackPluginScope::All` is what an UNBOUND track resolves to.
        let names: Vec<String> = plugin_tool_descriptors_from(
            registry.list(),
            &running(&[CONNECTOR_ID]),
            &TrackPluginScope::All,
        )
        .into_iter()
        .map(|d| d.name)
        .collect();

        assert!(
            names.contains(&format!("plugin.{CONNECTOR_ID}_{UNDERSCORE_TOOL}")),
            "allowlisted connector tool must be discoverable: {names:?}"
        );
        assert!(
            names.contains(&format!("plugin.{CONNECTOR_ID}_{OTHER_TOOL}")),
            "second allowlisted tool must be discoverable: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(DENIED_TOOL)),
            "a tool the upstream serves but `tools_allow` omits must NOT be \
             discoverable: {names:?}"
        );
    }

    #[test]
    fn stopped_connector_tools_are_invisible() {
        let registry = registry_after_materialization(&[(
            CONNECTOR_ID,
            &[UNDERSCORE_TOOL],
            &[UNDERSCORE_TOOL],
        )]);
        // Same registry, empty running set — the ONLY thing that changed.
        let names: Vec<String> =
            plugin_tool_descriptors_from(registry.list(), &running(&[]), &TrackPluginScope::All)
                .into_iter()
                .map(|d| d.name)
                .collect();
        assert!(
            names.is_empty(),
            "tools must vanish the instant the id leaves the running set: {names:?}"
        );
    }

    /// `entry is Found(e)` iff `route(plugin.{id}_{tool}) == Some((id, tool, e.kind))`.
    #[test]
    fn tool_entry_matches_tool_route() {
        let sibling = "mcp";
        let near_miss = format!("wisburg_{UNDERSCORE_TOOL}");
        let registry = PluginRegistry::from_manifests([
            (
                materialized_connector(
                    CONNECTOR_ID,
                    &[UNDERSCORE_TOOL, OTHER_TOOL],
                    &[UNDERSCORE_TOOL, OTHER_TOOL, DENIED_TOOL],
                ),
                None,
            ),
            (
                materialized_connector_schemaless(sibling, &[&near_miss], &[&near_miss]),
                None,
            ),
        ]);
        let pairs: Vec<(String, String)> = registry
            .list()
            .into_iter()
            .flat_map(|manifest| {
                let id = manifest.id.clone();
                manifest
                    .exposes_tools
                    .into_iter()
                    .map(move |tool| (id.clone(), tool.name))
            })
            .chain([
                (CONNECTOR_ID.to_string(), DENIED_TOOL.to_string()),
                (CONNECTOR_ID.to_string(), "no_such_tool".to_string()),
                ("nobody".to_string(), UNDERSCORE_TOOL.to_string()),
                (sibling.to_string(), UNDERSCORE_TOOL.to_string()),
            ])
            .collect();
        assert!(
            pairs.len() >= 7,
            "fixture must cover both plugins: {pairs:?}"
        );

        let running_sets = [
            running(&[]),
            running(&[CONNECTOR_ID]),
            running(&[sibling]),
            running(&[CONNECTOR_ID, sibling]),
        ];
        let mut found = 0usize;
        for running in &running_sets {
            for (id, tool) in &pairs {
                let entry = plugin_tool_entry(&registry, running, id, tool);
                let minted = format!("plugin.{id}_{tool}");
                let route = plugin_tool_route(&registry, &minted, running)
                    .unwrap_or_else(|e| panic!("{minted}: {e:?}"));
                match entry {
                    ToolEntry::Found(exposed) => {
                        found += 1;
                        assert_eq!(exposed.name, *tool);
                        assert_eq!(
                            route,
                            Some((id.clone(), tool.clone(), exposed.kind)),
                            "{minted} with running={running:?}: entry Found but route disagrees"
                        );
                    }
                    negative => assert_eq!(
                        route, None,
                        "{minted} with running={running:?}: entry {negative:?} but route hit"
                    ),
                }
            }
        }
        assert!(found > 0, "at least one pair must be Found");
    }

    #[test]
    fn underscore_bearing_connector_tool_routes_uniquely() {
        let registry = registry_after_materialization(&[(
            CONNECTOR_ID,
            &[UNDERSCORE_TOOL, OTHER_TOOL],
            &[UNDERSCORE_TOOL, OTHER_TOOL],
        )]);
        let minted = format!("plugin.{CONNECTOR_ID}_{UNDERSCORE_TOOL}");
        let route = plugin_tool_route(&registry, &minted, &running(&[CONNECTOR_ID]))
            .expect("route resolution must not be ambiguous")
            .expect("minted name must route");
        assert_eq!(route.0, CONNECTOR_ID);
        assert_eq!(route.1, UNDERSCORE_TOOL);
        // Connector tools are never forge actions; a `Some(ForgeAction)` would hand them the forge credential passthrough.
        assert!(route.2.is_none(), "connector tools must carry kind: None");
    }

    /// A sibling connector whose id is a strict PREFIX cannot collide: ids exclude `_`, so every minted name has exactly one possible split.
    #[test]
    fn prefix_sibling_connector_cannot_shadow_the_route() {
        let sibling = "mcp";
        let near_miss = format!("wisburg_{UNDERSCORE_TOOL}");
        let sibling_manifest =
            materialized_connector_schemaless(sibling, &[&near_miss], &[&near_miss]);
        assert_eq!(
            sibling_manifest.exposes_tools.len(),
            1,
            "sibling tool must materialize"
        );
        assert!(
            sibling_manifest.exposes_tools[0].input_schema.is_none(),
            "sibling upstream carries no `inputSchema` — keep this fixture \
             byte-identical to the pre-#1196 one"
        );
        let registry = PluginRegistry::from_manifests([
            (
                materialized_connector(CONNECTOR_ID, &[UNDERSCORE_TOOL], &[UNDERSCORE_TOOL]),
                None,
            ),
            (sibling_manifest, None),
        ]);

        let minted = format!("plugin.{CONNECTOR_ID}_{UNDERSCORE_TOOL}");
        let sibling_minted = format!("plugin.{sibling}_{near_miss}");
        assert_ne!(
            minted, sibling_minted,
            "the `_` boundary must keep these distinct"
        );

        let running = running(&[CONNECTOR_ID, sibling]);
        let route = plugin_tool_route(&registry, &minted, &running)
            .expect("must not be ambiguous")
            .expect("must route");
        assert_eq!(
            (route.0.as_str(), route.1.as_str()),
            (CONNECTOR_ID, UNDERSCORE_TOOL)
        );

        let sibling_route = plugin_tool_route(&registry, &sibling_minted, &running)
            .expect("must not be ambiguous")
            .expect("must route");
        assert_eq!(
            (sibling_route.0.as_str(), sibling_route.1.as_str()),
            (sibling, near_miss.as_str())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_result_filename_is_hash_based_and_path_safe() {
        let foo = forge_result_filename("foo");
        let foo_bar = forge_result_filename("foo.bar");
        let dot = forge_result_filename(".");
        let dotdot = forge_result_filename("..");

        assert_ne!(foo, foo_bar);
        assert_ne!(dot, dotdot);
        for filename in [foo, foo_bar, dot, dotdot] {
            assert_eq!(filename.len(), 71);
            assert!(filename.ends_with(".result"));
            assert!(
                Path::new(&filename)
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
            );
        }
    }

    #[test]
    fn semantic_forge_payload_hash_ignores_volatile_argv() {
        fn payload(argv: Vec<&str>, idem_key: &str) -> PluginForgePayload {
            PluginForgePayload {
                argv: argv.into_iter().map(str::to_string).collect(),
                idem_key: idem_key.into(),
                event_spec: None,
                subject: None,
                context: serde_json::Map::new(),
                probe: None,
                parked: true,
            }
        }

        let base = payload(vec!["gh", "pr", "merge", "42"], "gh.pr.merge:owner/repo:42");
        let edited_argv = PluginForgePayload {
            argv: vec!["gh", "pr", "merge", "42", "--squash", "--delete-branch"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            idem_key: "gh.pr.merge:owner/repo:42".into(),
            event_spec: None,
            subject: None,
            context: serde_json::Map::new(),
            probe: None,
            parked: true,
        };
        let different_identity =
            payload(vec!["gh", "pr", "merge", "43"], "gh.pr.merge:owner/repo:43");

        assert_eq!(
            semantic_payload_hash(&base).expect("hash base"),
            semantic_payload_hash(&edited_argv).expect("hash edited argv")
        );
        assert_ne!(
            semantic_payload_hash(&base).expect("hash base"),
            semantic_payload_hash(&different_identity).expect("hash different identity")
        );
    }
}

async fn card_bound_tool_identity(
    ctx: &Arc<AppContext>,
    bound: &CardIdentity,
) -> Result<ToolCallIdentity, RpcError> {
    let card = ensure_card_bound_session_active(ctx, bound, "tools/call").await?;
    Ok(ToolCallIdentity {
        card_id: card.card_id.as_str().to_string(),
        role: card.role,
        provider: bound.provider.clone(),
        session_id: bound.session_id.clone(),
        track_id: Some(card.track_id.as_str().to_string()),
        area_id: card.area_id.as_str().to_string(),
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
        || card.track_id != session.track_id
        || card.area_id.as_str() != bound.area_id.as_str()
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
        provider: runtime
            .agent_provider
            .clone()
            .unwrap_or(AgentProvider::Codex),
        session_id: runtime.id.clone(),
        track_id: Some(card.track_id.as_str().to_string()),
        area_id: card.area_id.as_str().to_string(),
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

/// Shared by production boot and integration tests.
pub(crate) fn default_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("mcp").join("kernel.sock")
}
