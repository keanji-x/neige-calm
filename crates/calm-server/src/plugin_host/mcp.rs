//! Line-delimited JSON-RPC 2.0 client actor for talking to a plugin process.
//!
//! Wire format: one JSON object per line, terminated by `\n`. Matches the MCP
//! `stdio` transport (modelcontextprotocol.io spec, 2025-11-25). NOT
//! Content-Length-framed — that's the HTTP transport, which we don't use.
//!
//! Topology (design doc §3.1): the kernel = MCP client, plugin = MCP server,
//! but the same socket carries plugin-initiated `neige.*` requests that the
//! kernel routes (Slice C). This module owns the framing + correlation +
//! `initialize` handshake; Slice C drains the inbound channels and dispatches.
//!
//! Concurrency model:
//!   * One reader task: parses each line, peels off whether it's a response
//!     (has `id`, may have `result` or `error`), a request (has `id` + `method`),
//!     or a notification (has `method`, no `id`). Routes accordingly.
//!   * One writer task: serializes outbound frames (one mpsc channel for both
//!     kernel-initiated requests and notifications, plus responses-to-plugin).
//!   * Public `McpClient` owns the outbound mpsc sender + the inflight-response
//!     map. `call` allocates an id, registers a oneshot, sends the frame, awaits.
//!
//! Channel sizes (design doc §3.2 — backpressure is fine):
//!   * outbound (kernel → plugin): 256 deep. Most calls are 1:1 RPC, so this
//!     bounds at the worst case of "burst of 256 notifications without flush".
//!   * inbound_requests / inbound_notifications: 64 deep. Slice C's dispatcher
//!     is the consumer; if it falls behind, we pause reading the plugin —
//!     better than dropping plugin → kernel writes silently.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::error::McpError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// MCP / kernel protocol version we advertise in `initialize`. Slice C will
/// likely move this into a shared constants module.
pub const KERNEL_PROTOCOL_VERSION: &str = "2025-11-25";

/// MCP `tools/call` content block — text / image / etc. We only carry the
/// fields we actually consume today (the `type` discriminator + `text`); other
/// fields (`data`, `mimeType`, ...) are tolerated under `extra` so future spec
/// extensions don't break parsing.
///
/// Spec ref: model-context-protocol 2025-11-25, `CallToolResult.content[]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Everything we don't model explicitly. Keeps unknown fields from
    /// breaking deserialization (the MCP Apps profile adds keys here over
    /// time).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// MCP `resources/read` content entry (M3). Spec 2026-01-26 §`ResourceContents`.
/// The kernel uses this for `ui://<plugin>/<view>` resources served from disk
/// (see `plugin_host::resources`). Either `text` or `blob` is populated per
/// the spec — we keep both `Option<String>` so we don't have to choose at
/// parse time, and we forward `_meta` verbatim so the MCP Apps profile's
/// `ui.csp` + `ui.permissions` round-trip without us pinning their shape.
///
/// Field naming: `mime_type` and `meta` rename to the camelCase wire keys
/// (`mimeType`, `_meta`) so Rust stays snake_case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// MCP `resources/read` result envelope. Per the spec, a single read may
/// return multiple `ResourceContent` blocks (e.g. multi-part documents); for
/// `ui://...` HTML resources the kernel always emits exactly one entry, but
/// we keep the Vec shape so consumers stay spec-compliant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceContents {
    #[serde(default)]
    pub contents: Vec<ResourceContent>,
}

/// MCP `tools/call` result shape (M2). Per spec 2025-11-25:
/// `{ content: ContentBlock[], isError?: bool, _meta?: object,
///    structuredContent?: any }`. We only inspect `_meta.ui.resourceUri` and
/// `is_error` for now — the rest is opaque pass-through to the caller.
///
/// Field naming: `is_error` and `structured_content` use serde renames to the
/// camelCase wire keys (`isError`, `structuredContent`) so our Rust code stays
/// in snake_case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(
        default,
        rename = "structuredContent",
        skip_serializing_if = "Option::is_none"
    )]
    pub structured_content: Option<Value>,
}

/// JSON-RPC `id`. Plugins occasionally use strings (the spec allows it), so
/// we accept either on the wire and re-serialize verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum RequestId {
    Num(u64),
    Str(String),
}

impl RequestId {
    fn from_u64(n: u64) -> Self {
        Self::Num(n)
    }
}

/// A plugin-originated JSON-RPC request the kernel must answer. Slice C will
/// match `method` against `neige.*` and route to a handler; for Slice B the
/// default consumer just responds `MethodNotFound` to everything so the wire
/// stays sane in tests.
#[derive(Debug)]
pub struct InboundRequest {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
    /// Slice C calls `responder.send(...)` with the kernel-side outcome. If
    /// dropped without sending, the reader task synthesizes a generic
    /// `InternalError` so the plugin doesn't deadlock.
    pub responder: oneshot::Sender<Result<Value, RpcError>>,
}

/// A plugin-originated notification (no response expected). Slice C drains
/// the receiver and acts (e.g. `notifications/cancelled`).
#[derive(Debug, Clone)]
pub struct InboundNotification {
    pub method: String,
    pub params: Value,
}

/// JSON-RPC `error` object per §5.1 of the spec. The `code` ranges and the
/// kernel-extension codes (-32001..-32005) are documented in design doc §3.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: Self::INTERNAL_ERROR,
            message: msg.into(),
            data: None,
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: Self::INVALID_PARAMS,
            message: msg.into(),
            data: None,
        }
    }

    pub fn custom(code: i64, msg: impl Into<String>) -> Self {
        Self {
            code,
            message: msg.into(),
            data: None,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "jsonrpc error {}: {}", self.code, self.message)
    }
}
impl std::error::Error for RpcError {}

// ---------------------------------------------------------------------------
// Outbound frame plumbing
// ---------------------------------------------------------------------------

/// A pre-serialized frame to send to the plugin. We pre-serialize so the
/// writer task is just "drain mpsc, write line, flush" — no JSON work on the
/// I/O path.
#[derive(Debug)]
struct OutboundFrame(Vec<u8>);

// ---------------------------------------------------------------------------
// McpClient — the public surface
// ---------------------------------------------------------------------------

type ResponderMap = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Wire-name of the experimental capability that opts a plugin into the
/// `neige.*` host-callback namespace. Plugins that never call back into the
/// kernel can omit this and still run; see `PluginHost::spawn` for the
/// gating logic.
pub const KERNEL_CALLBACKS_CAPABILITY: &str = "dev.neige/kernel-callbacks";

/// Version of the `dev.neige/kernel-callbacks` capability the kernel supports.
/// Plugins advertise `experimental[KERNEL_CALLBACKS_CAPABILITY].version` in
/// their `initialize` response; only an **exact** match here counts as
/// "capability declared". Any other value (including a missing `version`
/// field) is treated as the capability being absent and a `warn!` log is
/// emitted so the divergence is visible during debugging. Issue #45.
pub const KERNEL_CALLBACKS_CAPABILITY_VERSION: u32 = 1;

pub struct McpClient {
    next_id: AtomicU64,
    out_tx: mpsc::Sender<OutboundFrame>,
    /// Pending kernel → plugin requests waiting for a response.
    responders: ResponderMap,
    /// Reader task; aborted on drop so the runtime can collect the actor.
    reader_task: Mutex<Option<JoinHandle<()>>>,
    writer_task: Mutex<Option<JoinHandle<()>>>,
    /// Set on transport-level errors. Future calls fail fast instead of
    /// hanging on a oneshot that nobody will ever fulfill.
    closed: Arc<AsyncMutex<Option<String>>>,
    /// Inbound channels — handed out exactly once via the take_* methods.
    /// Wrapped in `Mutex<Option<...>>` so the actor's reader task can own its
    /// half of the channel and any single consumer can take the receiver.
    inbound_requests_rx: Mutex<Option<mpsc::Receiver<InboundRequest>>>,
    inbound_notifications_rx: Mutex<Option<mpsc::Receiver<InboundNotification>>>,
    /// `result.capabilities` from the plugin's `initialize` response — captured
    /// once during handshake. Plugins opt into `neige.*` host-callbacks by
    /// echoing back the kernel's `experimental.dev.neige/kernel-callbacks`
    /// entry; the host inspects this via `server_capabilities()` to decide
    /// whether to install the dispatcher or a MethodNotFound drainer.
    server_capabilities: Mutex<Value>,
}

impl McpClient {
    /// Spawn reader + writer tasks over the given stream pair and perform the
    /// `initialize` handshake. Returns once initialize succeeds (or fails with
    /// `InitializeRejected`-equivalent semantics in `McpError`).
    ///
    /// `read`/`write` are typically `tokio::process::ChildStdout` and
    /// `ChildStdin` respectively, but we keep them `dyn` so tests can wire in
    /// `tokio::io::duplex` halves without spawning a real process.
    ///
    /// When `expected_echo` is `Some(raw_token)`, the kernel embeds the raw
    /// token in `initialize.params._meta["dev.neige/auth"].expected_echo` and
    /// requires the plugin to mirror it back in
    /// `initialize.result._meta["dev.neige/auth"].echoed_token`. Mismatch
    /// surfaces as `McpError::Framing("auth mismatch")` so callers
    /// (`PluginHost::spawn`) can translate to `HostError::AuthMismatch` and
    /// skip respawn.
    ///
    /// `None` skips the check entirely — only used by unit tests that wire a
    /// plain duplex stub. Production always passes the per-process token.
    pub async fn connect_with_auth<R, W>(
        read: R,
        write: W,
        expected_echo: Option<&str>,
    ) -> Result<Arc<Self>, McpError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<OutboundFrame>(256);
        let (in_req_tx, in_req_rx) = mpsc::channel::<InboundRequest>(64);
        let (in_notif_tx, in_notif_rx) = mpsc::channel::<InboundNotification>(64);

        let responders: ResponderMap = Arc::new(Mutex::new(HashMap::new()));
        let closed: Arc<AsyncMutex<Option<String>>> = Arc::new(AsyncMutex::new(None));

        let writer_task = spawn_writer(write, out_rx, closed.clone());
        let reader_task = spawn_reader(
            read,
            responders.clone(),
            in_req_tx,
            in_notif_tx,
            out_tx.clone(),
            closed.clone(),
        );

        let client = Arc::new(Self {
            next_id: AtomicU64::new(1),
            out_tx,
            responders,
            reader_task: Mutex::new(Some(reader_task)),
            writer_task: Mutex::new(Some(writer_task)),
            closed,
            inbound_requests_rx: Mutex::new(Some(in_req_rx)),
            inbound_notifications_rx: Mutex::new(Some(in_notif_rx)),
            server_capabilities: Mutex::new(Value::Object(Default::default())),
        });

        // initialize handshake — design doc §3.1.
        client.initialize(expected_echo).await?;

        Ok(client)
    }

    /// `initialize` request per MCP spec: declare protocol version + the
    /// `experimental.dev.neige/kernel-callbacks` capability (design doc §3.1,
    /// migration doc §6/M1) so the plugin knows we accept `neige.*` callbacks
    /// and can opt in by echoing the capability back. We don't read every
    /// `serverInfo` field — just confirm a `protocolVersion` echo and that the
    /// response is shaped like an object.
    ///
    /// Auth handshake (migration doc §7.6 row 2): the auth-echo lives at
    /// `params._meta["dev.neige/auth"].expected_echo` and the plugin must
    /// mirror it back at `result._meta["dev.neige/auth"].echoed_token`.
    ///
    /// When `expected_echo` is `Some(raw)`, we inline the raw token under
    /// `_meta` and demand the plugin echo it back. The kernel-side raw-vs-raw
    /// equality is fine here because the token is full-entropy and the
    /// mismatch path kills the process immediately.
    async fn initialize(self: &Arc<Self>, expected_echo: Option<&str>) -> Result<(), McpError> {
        let mut params = json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: { "version": 1 }
                }
            },
            "clientInfo": {
                "name": "neige-calm-server",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        if let Some(raw) = expected_echo {
            params["_meta"] = json!({
                "dev.neige/auth": { "expected_echo": raw }
            });
        }

        // Bound the handshake — a healthy stub responds in < 50 ms; we give
        // 10 s for slow CI / cold-start cases.
        let result =
            tokio::time::timeout(Duration::from_secs(10), self.call("initialize", params)).await;

        let value = match result {
            Ok(Ok(v)) => v,
            Ok(Err(rpc)) => {
                return Err(McpError::TransportClosed(format!(
                    "initialize rejected by plugin: {rpc}"
                )));
            }
            Err(_timeout) => {
                return Err(McpError::TransportClosed(
                    "initialize timed out waiting for plugin response".into(),
                ));
            }
        };

        if !value.is_object() {
            return Err(McpError::Framing(format!(
                "initialize result was not an object: {value}"
            )));
        }
        // Validate `serverInfo` is *some* object if present — be lenient about
        // missing optional fields; the spec lets implementations evolve.
        if let Some(server_info) = value.get("serverInfo")
            && !server_info.is_object()
        {
            return Err(McpError::Framing(format!(
                "initialize.serverInfo was not an object: {server_info}"
            )));
        }
        // Issue #45: enforce that the plugin echoes the exact protocol version
        // the kernel advertised. Pre-#45 the kernel sent `KERNEL_PROTOCOL_VERSION`
        // but accepted any (or missing) `result.protocolVersion`; a plugin
        // claiming an incompatible spec revision would silently negotiate
        // against a kernel that doesn't actually speak its dialect. We now
        // fail the handshake on mismatch — the existing initialize-failure
        // path reaps the child and surfaces `HostError::InitializeRejected`.
        let plugin_protocol = value
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if plugin_protocol != KERNEL_PROTOCOL_VERSION {
            return Err(McpError::ProtocolVersionMismatch {
                kernel: KERNEL_PROTOCOL_VERSION.to_string(),
                plugin: plugin_protocol.to_string(),
            });
        }
        // M1: when we issued an expected_echo, the plugin must mirror it back
        // at `result._meta["dev.neige/auth"].echoed_token`. Use the marker
        // string `auth mismatch` so PluginHost::spawn can recognize the path
        // and translate to HostError::AuthMismatch.
        if let Some(expected) = expected_echo {
            let echoed = value
                .pointer("/_meta/dev.neige~1auth/echoed_token")
                .and_then(|v| v.as_str());
            match echoed {
                Some(got) if got == expected => {}
                _ => {
                    return Err(McpError::Framing("auth mismatch".into()));
                }
            }
        }
        // M1: capture `result.capabilities` for later host-side gating of the
        // `neige.*` router (`PluginHost::spawn` reads it via
        // `server_capabilities()`). Missing/non-object → empty object, which
        // surfaces as "no capability declared".
        let caps = value
            .get("capabilities")
            .cloned()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| Value::Object(Default::default()));
        *self.server_capabilities.lock().unwrap() = caps;
        // After initialize, MCP spec wants a `notifications/initialized` from
        // the client. Fire-and-forget.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// `result.capabilities` from the most recent successful `initialize`
    /// response. Empty object until the handshake completes. Slice C (now M1)
    /// reads `experimental[KERNEL_CALLBACKS_CAPABILITY]` to decide whether to
    /// install the `neige.*` dispatcher or a MethodNotFound drainer.
    pub fn server_capabilities(&self) -> Value {
        self.server_capabilities.lock().unwrap().clone()
    }

    /// Convenience predicate: did the plugin opt into the `neige.*` namespace?
    /// True iff
    /// `result.capabilities.experimental["dev.neige/kernel-callbacks"].version`
    /// equals `KERNEL_CALLBACKS_CAPABILITY_VERSION`. Any other value
    /// (including a missing entry or a missing `version` field) is treated
    /// as the capability being absent. When the capability is present but
    /// at a non-matching version, a `warn!` log is emitted so the divergence
    /// is visible during debugging. Issue #45.
    pub fn has_kernel_callbacks_capability(&self, plugin_id: &str) -> bool {
        let caps = self.server_capabilities.lock().unwrap();
        let entry = caps.pointer(&format!(
            "/experimental/{}",
            KERNEL_CALLBACKS_CAPABILITY.replace('/', "~1")
        ));
        match entry {
            None => false,
            Some(node) => {
                // Per spec the value is an object with a `version` field. We
                // accept only `u32` exact-match; anything else (missing field,
                // wrong type, wrong number) → treat as absent and warn so
                // operators can see the version skew.
                let version = node.get("version").and_then(|v| v.as_u64());
                match version {
                    Some(v) if v == u64::from(KERNEL_CALLBACKS_CAPABILITY_VERSION) => true,
                    _ => {
                        tracing::warn!(
                            plugin_id = %plugin_id,
                            advertised = ?node.get("version"),
                            expected = KERNEL_CALLBACKS_CAPABILITY_VERSION,
                            "plugin advertised experimental.dev.neige/kernel-callbacks with \
                             non-matching version; treating as absent"
                        );
                        false
                    }
                }
            }
        }
    }

    /// MCP `tools/call` (M2): invoke a tool the plugin server registered via
    /// `tools/list`. Returns the parsed `CallToolResult`. Errors surface as
    /// either a `RpcError` (transport-level / spec-shaped) or — when the tool
    /// itself signalled failure — `result.is_error == Some(true)` with
    /// human-readable text in `result.content` for the caller to relay.
    ///
    /// We keep the result type loose: `_meta` is `serde_json::Value` so the
    /// host route can pluck `_meta.ui.resourceUri` (the M2 use case) without
    /// us pinning every reserved sub-key.
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, RpcError> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });
        let raw = self.call("tools/call", params).await?;
        serde_json::from_value::<CallToolResult>(raw).map_err(|e| {
            RpcError::internal(format!(
                "tools/call: response did not parse as CallToolResult: {e}"
            ))
        })
    }

    /// MCP `resources/read` (M3): fetch a resource by URI. Pattern-mirror of
    /// `tools_call`. Returns the parsed `ResourceContents` (one or more entries
    /// per the spec). Note that `ui://<plugin>/<view>` resources are served by
    /// the *kernel*, not the plugin — this method is the plugin-facing
    /// counterpart for plugin-owned resource URIs (e.g. `neige://...` reads).
    /// The kernel-side `ui://` handler lives in `plugin_host::resources`.
    pub async fn resources_read(&self, uri: &str) -> Result<ResourceContents, RpcError> {
        let params = json!({ "uri": uri });
        let raw = self.call("resources/read", params).await?;
        serde_json::from_value::<ResourceContents>(raw).map_err(|e| {
            RpcError::internal(format!(
                "resources/read: response did not parse as ResourceContents: {e}"
            ))
        })
    }

    /// Outbound call (kernel → plugin). Allocates an id, registers a oneshot,
    /// sends the frame, awaits the matching response.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        // Cheap fast-path: if transport is already closed, surface that as an
        // internal error instead of registering a doomed responder.
        if let Some(reason) = self.closed.lock().await.clone() {
            return Err(RpcError::internal(format!(
                "mcp transport closed: {reason}"
            )));
        }

        let id = RequestId::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.responders.lock().unwrap().insert(id.clone(), tx);

        let frame = build_request_frame(&id, method, &params);
        if self.out_tx.send(OutboundFrame(frame)).await.is_err() {
            // Writer task is gone. Drop our responder slot so we don't leak.
            self.responders.lock().unwrap().remove(&id);
            return Err(RpcError::internal("mcp writer task gone"));
        }

        match rx.await {
            Ok(res) => res,
            Err(_) => Err(RpcError::internal("response channel dropped")),
        }
    }

    /// Outbound notification (kernel → plugin). No response expected.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        if let Some(reason) = self.closed.lock().await.clone() {
            return Err(McpError::TransportClosed(reason));
        }
        let frame = build_notification_frame(method, &params);
        self.out_tx
            .send(OutboundFrame(frame))
            .await
            .map_err(|_| McpError::TransportClosed("writer task gone".into()))?;
        Ok(())
    }

    /// Take the inbound-request channel. Slice C calls this once at host
    /// init and drains. Subsequent calls return `None`.
    ///
    /// Slice B's tests call this and drain with a no-op responder so the
    /// channel doesn't backpressure on noisy plugins.
    pub fn take_inbound_requests(&self) -> Option<mpsc::Receiver<InboundRequest>> {
        self.inbound_requests_rx.lock().unwrap().take()
    }

    /// Take the inbound-notification channel.
    pub fn take_inbound_notifications(&self) -> Option<mpsc::Receiver<InboundNotification>> {
        self.inbound_notifications_rx.lock().unwrap().take()
    }

    /// True once the reader or writer task has flagged the transport closed.
    pub fn is_closed(&self) -> bool {
        // try_lock is a non-blocking peek; in the unlikely race we return
        // `false` and the next call will surface the actual state.
        self.closed.try_lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut t) = self.reader_task.lock()
            && let Some(h) = t.take()
        {
            h.abort();
        }
        if let Ok(mut t) = self.writer_task.lock()
            && let Some(h) = t.take()
        {
            h.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

fn spawn_writer<W>(
    mut write: W,
    mut rx: mpsc::Receiver<OutboundFrame>,
    closed: Arc<AsyncMutex<Option<String>>>,
) -> JoinHandle<()>
where
    W: AsyncWrite + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(e) = write.write_all(&frame.0).await {
                let reason = format!("write failed: {e}");
                *closed.lock().await = Some(reason);
                return;
            }
            // We append `\n` in the frame builder, so flush per-message is the
            // simplest "send immediately" guarantee. For high throughput we'd
            // batch; for plugin RPC, latency wins over throughput.
            if let Err(e) = write.flush().await {
                let reason = format!("flush failed: {e}");
                *closed.lock().await = Some(reason);
                return;
            }
        }
        // mpsc closed — nothing left to write.
    })
}

fn spawn_reader<R>(
    read: R,
    responders: ResponderMap,
    in_req_tx: mpsc::Sender<InboundRequest>,
    in_notif_tx: mpsc::Sender<InboundNotification>,
    out_tx: mpsc::Sender<OutboundFrame>,
    closed: Arc<AsyncMutex<Option<String>>>,
) -> JoinHandle<()>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        // BufReader<R> implements AsyncBufReadExt directly when R: AsyncRead +
        // Unpin. Trying to abstract via a trait object trips dyn-compatibility
        // (read_line returns ReadLine<'_, Self>); keep it monomorphized.
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => {
                    let reason = format!("read failed: {e}");
                    *closed.lock().await = Some(reason);
                    flush_responders_with_error(&responders, "mcp read failed");
                    return;
                }
            };
            if n == 0 {
                // EOF — plugin closed stdout (likely exited).
                *closed.lock().await = Some("eof".into());
                flush_responders_with_error(&responders, "mcp transport eof");
                return;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match parse_frame(trimmed) {
                Ok(Frame::Response { id, body }) => {
                    if let Some(slot) = responders.lock().unwrap().remove(&id) {
                        let _ = slot.send(body);
                    } else {
                        tracing::warn!(?id, "mcp response for unknown request id; dropping");
                    }
                }
                Ok(Frame::Request { id, method, params }) => {
                    let (responder_tx, responder_rx) = oneshot::channel();
                    let req = InboundRequest {
                        id: id.clone(),
                        method: method.clone(),
                        params,
                        responder: responder_tx,
                    };
                    // If Slice C hasn't drained yet, we block here — that's the
                    // intentional backpressure. The writer can still send
                    // responses for in-flight kernel→plugin requests because
                    // the writer task is a separate channel.
                    if in_req_tx.send(req).await.is_err() {
                        // Nobody is listening on the request channel (test
                        // setup where the consumer was dropped). Synthesize a
                        // MethodNotFound so the plugin doesn't hang.
                        let frame =
                            build_error_response_frame(&id, &RpcError::method_not_found(&method));
                        let _ = out_tx.send(OutboundFrame(frame)).await;
                        continue;
                    }
                    // Spawn a small joiner so a stuck handler can't block the
                    // reader. The responder oneshot is a single value, so this
                    // task lifetime equals "until Slice C answers or drops".
                    let out_tx2 = out_tx.clone();
                    let id2 = id.clone();
                    let method2 = method.clone();
                    tokio::spawn(async move {
                        let frame = match responder_rx.await {
                            Ok(Ok(value)) => build_ok_response_frame(&id2, &value),
                            Ok(Err(rpc)) => build_error_response_frame(&id2, &rpc),
                            Err(_) => build_error_response_frame(
                                &id2,
                                &RpcError::internal(format!(
                                    "kernel handler for {method2} dropped without responding"
                                )),
                            ),
                        };
                        let _ = out_tx2.send(OutboundFrame(frame)).await;
                    });
                }
                Ok(Frame::Notification { method, params }) => {
                    let notif = InboundNotification { method, params };
                    // Bounded mpsc — drop if Slice C isn't draining. Notifs
                    // are by spec lossy, so silent drop is correct semantics.
                    if let Err(e) = in_notif_tx.try_send(notif) {
                        tracing::debug!(error = %e, "inbound notification dropped (buffer full or no consumer)");
                    }
                }
                Err(e) => {
                    tracing::warn!(line = %trimmed, error = %e, "mcp framing error; ignoring line");
                }
            }
        }
    })
}

fn flush_responders_with_error(responders: &ResponderMap, msg: &str) {
    let mut map = responders.lock().unwrap();
    let drained: Vec<_> = map.drain().collect();
    drop(map);
    for (_id, slot) in drained {
        let _ = slot.send(Err(RpcError::internal(msg)));
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Frame {
    Response {
        id: RequestId,
        body: Result<Value, RpcError>,
    },
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

fn parse_frame(s: &str) -> Result<Frame, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| format!("json parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "frame is not an object".to_string())?;

    // Per spec, `jsonrpc` MUST be "2.0". We accept missing for ergonomic stubs
    // but warn — production plugins should send it.
    let _jsonrpc = obj.get("jsonrpc");

    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(|v| v.as_str()).map(String::from);

    match (id, method) {
        (Some(id_v), Some(m)) => {
            // Request: has id + method.
            let id = serde_json::from_value::<RequestId>(id_v.clone())
                .map_err(|e| format!("invalid id: {e}"))?;
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            Ok(Frame::Request {
                id,
                method: m,
                params,
            })
        }
        (Some(id_v), None) => {
            // Response: has id, no method. Should have `result` xor `error`.
            let id = serde_json::from_value::<RequestId>(id_v.clone())
                .map_err(|e| format!("invalid id: {e}"))?;
            if let Some(err_v) = obj.get("error") {
                let rpc: RpcError = serde_json::from_value(err_v.clone())
                    .map_err(|e| format!("invalid error object: {e}"))?;
                Ok(Frame::Response { id, body: Err(rpc) })
            } else if let Some(result_v) = obj.get("result") {
                Ok(Frame::Response {
                    id,
                    body: Ok(result_v.clone()),
                })
            } else {
                Err("response has neither result nor error".into())
            }
        }
        (None, Some(m)) => {
            // Notification: method, no id.
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            Ok(Frame::Notification { method: m, params })
        }
        (None, None) => Err("frame has neither id nor method".into()),
    }
}

fn build_request_frame(id: &RequestId, method: &str, params: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

fn build_notification_frame(method: &str, params: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

fn build_ok_response_frame(id: &RequestId, result: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

fn build_error_response_frame(id: &RequestId, err: &RpcError) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

// ===========================================================================
// Unit tests — framing only. End-to-end actor tests live in the smoke test.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_frame_round_trip() {
        let f = parse_frame(r#"{"jsonrpc":"2.0","id":7,"method":"foo","params":{"a":1}}"#).unwrap();
        match f {
            Frame::Request { id, method, params } => {
                assert_eq!(id, RequestId::Num(7));
                assert_eq!(method, "foo");
                assert_eq!(params["a"], 1);
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn parse_response_with_result() {
        let f = parse_frame(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        match f {
            Frame::Response {
                id,
                body: Ok(value),
            } => {
                assert_eq!(id, RequestId::Num(1));
                assert_eq!(value["ok"], true);
            }
            _ => panic!("expected ok response"),
        }
    }

    #[test]
    fn parse_response_with_error() {
        let f =
            parse_frame(r#"{"jsonrpc":"2.0","id":"abc","error":{"code":-32601,"message":"nope"}}"#)
                .unwrap();
        match f {
            Frame::Response { id, body: Err(rpc) } => {
                assert_eq!(id, RequestId::Str("abc".into()));
                assert_eq!(rpc.code, -32601);
                assert_eq!(rpc.message, "nope");
            }
            _ => panic!("expected error response"),
        }
    }

    #[test]
    fn parse_notification() {
        let f = parse_frame(r#"{"jsonrpc":"2.0","method":"tick","params":[1,2]}"#).unwrap();
        match f {
            Frame::Notification { method, params } => {
                assert_eq!(method, "tick");
                assert_eq!(params, json!([1, 2]));
            }
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn parse_garbage_errors() {
        assert!(parse_frame("not json").is_err());
        assert!(parse_frame("[1,2,3]").is_err());
        assert!(parse_frame(r#"{"jsonrpc":"2.0"}"#).is_err());
        assert!(parse_frame(r#"{"jsonrpc":"2.0","id":1}"#).is_err()); // no result/error
    }

    #[tokio::test]
    async fn tools_call_parses_result_meta_ui() {
        // M2: assert McpClient::tools_call serializes the right wire shape and
        // parses _meta.ui.resourceUri + structuredContent into CallToolResult.
        let (kernel, plugin) = tokio::io::duplex(8 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);

        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(p_r);
            let mut writer = p_w;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let v: Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = match v.get("id").cloned() {
                    Some(i) => i,
                    None => continue,
                };
                let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let reply = if method == "initialize" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": KERNEL_PROTOCOL_VERSION,
                            "serverInfo": { "name": "stub", "version": "0.0.0" },
                            "capabilities": {}
                        }
                    })
                } else if method == "tools/call" {
                    // Assert the kernel sent the expected shape so we know
                    // tools_call's serialization matches the spec.
                    let params = v.get("params").expect("params");
                    assert_eq!(params["name"], "make_status_card");
                    assert_eq!(params["arguments"]["x"], 1);
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [],
                            "isError": false,
                            "_meta": {
                                "ui": { "resourceUri": "ui://stub/status" }
                            },
                            "structuredContent": { "msg": "hi" }
                        }
                    })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                };
                let mut s = serde_json::to_string(&reply).unwrap();
                s.push('\n');
                writer.write_all(s.as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
            }
        });

        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        let result = client
            .tools_call("make_status_card", json!({ "x": 1 }))
            .await
            .expect("tools_call");
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|m| m.pointer("/ui/resourceUri"))
                .and_then(|v| v.as_str()),
            Some("ui://stub/status")
        );
        assert_eq!(result.structured_content, Some(json!({ "msg": "hi" })));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), plugin_task).await;
    }

    #[tokio::test]
    async fn resources_read_parses_contents_with_meta_ui() {
        // M3: assert McpClient::resources_read serializes the spec wire shape
        // and parses contents[].{uri, mimeType, text, _meta} into
        // ResourceContents.
        let (kernel, plugin) = tokio::io::duplex(8 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);

        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(p_r);
            let mut writer = p_w;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let v: Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = match v.get("id").cloned() {
                    Some(i) => i,
                    None => continue,
                };
                let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let reply = if method == "initialize" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": KERNEL_PROTOCOL_VERSION,
                            "serverInfo": { "name": "stub", "version": "0.0.0" },
                            "capabilities": {}
                        }
                    })
                } else if method == "resources/read" {
                    let params = v.get("params").expect("params");
                    assert_eq!(params["uri"], "ui://stub/status");
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "contents": [{
                                "uri": "ui://stub/status",
                                "mimeType": "text/html;profile=mcp-app",
                                "text": "<html>ok</html>",
                                "_meta": {
                                    "ui": {
                                        "csp": { "default_src": ["'self'"] },
                                        "permissions": { "tools": ["neige.overlay.set"] }
                                    }
                                }
                            }]
                        }
                    })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                };
                let mut s = serde_json::to_string(&reply).unwrap();
                s.push('\n');
                writer.write_all(s.as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
            }
        });

        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        let result = client
            .resources_read("ui://stub/status")
            .await
            .expect("resources_read");
        assert_eq!(result.contents.len(), 1);
        let entry = &result.contents[0];
        assert_eq!(entry.uri, "ui://stub/status");
        assert_eq!(
            entry.mime_type.as_deref(),
            Some("text/html;profile=mcp-app")
        );
        assert_eq!(entry.text.as_deref(), Some("<html>ok</html>"));
        assert!(entry.blob.is_none());
        assert_eq!(
            entry
                .meta
                .as_ref()
                .and_then(|m| m.pointer("/ui/csp/default_src/0"))
                .and_then(|v| v.as_str()),
            Some("'self'")
        );
        assert_eq!(
            entry
                .meta
                .as_ref()
                .and_then(|m| m.pointer("/ui/permissions/tools/0"))
                .and_then(|v| v.as_str()),
            Some("neige.overlay.set")
        );
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), plugin_task).await;
    }

    #[tokio::test]
    async fn client_round_trips_one_call() {
        // Stub the plugin as another duplex half. We "echo": for any incoming
        // request, reply with `{"got": method}`. We *never* send our own
        // requests, so initialize's notifications/initialized notification
        // is harmless on this end.
        let (kernel, plugin) = tokio::io::duplex(8 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);

        // Stub plugin task: parse lines, respond to anything with an id.
        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(p_r);
            let mut writer = p_w;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                let v: Value = serde_json::from_str(buf.trim()).unwrap();
                if let Some(id) = v.get("id").cloned() {
                    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let reply = if method == "initialize" {
                        // Reply with a serverInfo so initialize succeeds.
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": KERNEL_PROTOCOL_VERSION,
                                "serverInfo": { "name": "stub", "version": "0.0.0" },
                                "capabilities": {}
                            }
                        })
                    } else {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "got": method }
                        })
                    };
                    let mut s = serde_json::to_string(&reply).unwrap();
                    s.push('\n');
                    writer.write_all(s.as_bytes()).await.unwrap();
                    writer.flush().await.unwrap();
                }
                // notifications: no response.
            }
        });

        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        let result = client.call("hello", json!({})).await.expect("call");
        assert_eq!(result["got"], "hello");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), plugin_task).await;
    }

    /// Issue #45 test helper: spawn a stub plugin that replies to `initialize`
    /// with a caller-supplied `result` payload, then echoes any further
    /// requests as `{"echo": method}`. Returns the duplex halves the kernel
    /// side should hand to `McpClient::connect_with_auth`, plus the JoinHandle so tests
    /// can drain it on shutdown.
    fn spawn_init_stub(
        init_result: Value,
    ) -> (
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
        JoinHandle<()>,
    ) {
        let (kernel, plugin) = tokio::io::duplex(8 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut reader = BufReader::new(p_r);
            let mut writer = p_w;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let v: Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = match v.get("id").cloned() {
                    Some(i) => i,
                    None => continue,
                };
                let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let reply = if method == "initialize" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": init_result,
                    })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "echo": method }
                    })
                };
                let mut s = serde_json::to_string(&reply).unwrap();
                s.push('\n');
                if writer.write_all(s.as_bytes()).await.is_err() {
                    return;
                }
                if writer.flush().await.is_err() {
                    return;
                }
            }
        });
        (k_r, k_w, task)
    }

    #[tokio::test]
    async fn initialize_accepts_matching_protocol_version() {
        // Issue #45: when the plugin echoes the kernel's `KERNEL_PROTOCOL_VERSION`
        // verbatim, handshake should succeed (this is the green-path baseline
        // every existing stub fixture already meets).
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        // sanity: with empty capabilities, the kernel-callbacks predicate
        // returns false (capability absent).
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn initialize_rejects_mismatched_protocol_version() {
        // Issue #45: a plugin claiming a different `protocolVersion` must fail
        // the handshake with the typed `ProtocolVersionMismatch` variant.
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": "2099-01-01",
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        match McpClient::connect_with_auth(k_r, k_w, None).await {
            Ok(_) => panic!("handshake should fail on protocol mismatch"),
            Err(McpError::ProtocolVersionMismatch { kernel, plugin }) => {
                assert_eq!(kernel, KERNEL_PROTOCOL_VERSION);
                assert_eq!(plugin, "2099-01-01");
            }
            Err(other) => panic!("expected ProtocolVersionMismatch, got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_present_with_matching_version_is_true() {
        // Issue #45: `version: KERNEL_CALLBACKS_CAPABILITY_VERSION` → present.
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: { "version": KERNEL_CALLBACKS_CAPABILITY_VERSION }
                }
            }
        }));
        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        assert!(client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_present_with_wrong_version_is_false() {
        // Issue #45: `version: 2` (or anything ≠ KERNEL_CALLBACKS_CAPABILITY_VERSION)
        // → treated as absent. A warn-level log is emitted; we don't assert on
        // the log output (no log-capture infra in this crate yet) but the
        // boolean is the load-bearing contract for the host gating logic.
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: { "version": 2 }
                }
            }
        }));
        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_present_without_version_field_is_false() {
        // Issue #45: object present but `version` missing → absent.
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: {}
                }
            }
        }));
        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_entirely_absent_is_false() {
        // Issue #45: no `experimental` entry at all → absent (regression guard
        // for the pre-#45 behavior that the no-capability gating tests already
        // depend on; explicit here to keep all six cases co-located).
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        let client = McpClient::connect_with_auth(k_r, k_w, None).await.expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }
}
