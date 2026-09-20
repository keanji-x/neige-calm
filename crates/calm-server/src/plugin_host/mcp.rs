//! Line-delimited JSON-RPC 2.0 client actor for talking to a plugin process (MCP `stdio`
//! transport: one JSON object per line, not Content-Length-framed).

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

/// MCP / kernel protocol version we advertise in `initialize`.
pub const KERNEL_PROTOCOL_VERSION: &str = "2025-11-25";

/// MCP `tools/call` content block; unknown fields are tolerated under `extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Everything we don't model explicitly, so unknown fields don't break deserialization.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// MCP `resources/read` content entry. `_meta` is forwarded verbatim so the MCP Apps profile's
/// `ui.csp` + `ui.permissions` round-trip without pinning their shape.
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

/// MCP `resources/read` result envelope; a single read may return multiple entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceContents {
    #[serde(default)]
    pub contents: Vec<ResourceContent>,
}

/// MCP `tools/call` result shape. Only `_meta.ui.resourceUri` and `is_error` are inspected; the rest is pass-through.
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

/// JSON-RPC `id`. Plugins occasionally use strings (the specification allows it), so
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

/// A plugin-originated JSON-RPC request the kernel must answer.
#[derive(Debug)]
pub struct InboundRequest {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
    /// If dropped without sending, the reader task synthesizes a generic `InternalError` so the plugin doesn't deadlock.
    pub responder: oneshot::Sender<Result<Value, RpcError>>,
}

/// A plugin-originated notification (no response expected).
#[derive(Debug, Clone)]
pub struct InboundNotification {
    pub method: String,
    pub params: Value,
}

/// JSON-RPC `error` object.
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

/// A pre-serialized frame to send to the plugin, so the writer task does no JSON work on the I/O path.
#[derive(Debug)]
struct OutboundFrame(Vec<u8>);

type ResponderMap = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, RpcError>>>>>;

/// RAII ownership of one entry in the [`ResponderMap`] for the lifetime of the `call` future that
/// registered it; dropping the guard (including cancellation by a timeout) removes the entry.
struct ResponderSlot<'a> {
    map: &'a ResponderMap,
    id: RequestId,
}

impl Drop for ResponderSlot<'_> {
    fn drop(&mut self) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// Wire-name of the experimental capability that opts a plugin into the `neige.*` host-callback namespace.
pub const KERNEL_CALLBACKS_CAPABILITY: &str = "dev.neige/kernel-callbacks";

/// `_meta` namespace naming the Track a `tools/call` was made from, as `{"id": "<track_id>"}`.
/// The kernel fills this from the resolved identity; nothing in the request body reaches it. Not every
/// `tools/call` carries it, so a plugin must refuse rather than default when it is absent.
pub const TRACK_META_KEY: &str = "dev.neige/track";

/// Version of the `dev.neige/kernel-callbacks` capability; only an exact match in the plugin's
/// `initialize` response counts as "capability declared".
pub const KERNEL_CALLBACKS_CAPABILITY_VERSION: u32 = 1;

/// Everything the kernel hands a plugin inside `initialize.params._meta`. Deliberately no `Default`:
/// every field is mandatory at every call site so a handshake that delivers no config is written on purpose.
#[derive(Clone, Copy)]
pub struct InitializeMeta<'a> {
    /// The raw per-process token the plugin must mirror back at `result._meta["dev.neige/auth"].echoed_token`;
    /// `None` skips the check (unit tests only).
    pub expected_echo: Option<&'a str>,
    /// `defaults ⊕ user_config` for this plugin. `None` omits the namespace; `Some(empty)` states
    /// "this kernel delivers configuration and there is none".
    pub config: Option<&'a serde_json::Map<String, Value>>,
}

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
    inbound_requests_rx: Mutex<Option<mpsc::Receiver<InboundRequest>>>,
    inbound_notifications_rx: Mutex<Option<mpsc::Receiver<InboundNotification>>>,
    /// `result.capabilities` from the plugin's `initialize` response, captured once during handshake.
    server_capabilities: Mutex<Value>,
}

impl McpClient {
    /// Spawn reader + writer tasks over the given stream pair and perform the `initialize` handshake.
    /// An auth-echo mismatch surfaces as `McpError::Framing("auth mismatch")`.
    pub async fn connect_with_auth<R, W>(
        read: R,
        write: W,
        meta: InitializeMeta<'_>,
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

        client.initialize(meta).await?;

        Ok(client)
    }

    /// `initialize` request. `_meta` carries the auth echo and the config as `{"values": {…}}`; the
    /// wrapper lets the kernel add sibling fields without colliding with a configuration key. Config is not echoed back.
    async fn initialize(self: &Arc<Self>, meta: InitializeMeta<'_>) -> Result<(), McpError> {
        let InitializeMeta {
            expected_echo,
            config,
        } = meta;
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
        {
            let mut meta_obj = serde_json::Map::new();
            if let Some(raw) = expected_echo {
                meta_obj.insert("dev.neige/auth".into(), json!({ "expected_echo": raw }));
            }
            if let Some(values) = config {
                meta_obj.insert(
                    "dev.neige/config".into(),
                    json!({ "values": Value::Object(values.clone()) }),
                );
            }
            if !meta_obj.is_empty() {
                params["_meta"] = Value::Object(meta_obj);
            }
        }

        // 10 s bounds the handshake for slow CI / cold-start cases.
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
        if let Some(server_info) = value.get("serverInfo")
            && !server_info.is_object()
        {
            return Err(McpError::Framing(format!(
                "initialize.serverInfo was not an object: {server_info}"
            )));
        }
        // The plugin must echo the exact protocol version the kernel advertised.
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
        // The `auth mismatch` marker string is what `PluginHost::spawn` recognizes to translate to `HostError::AuthMismatch`.
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
        // Missing/non-object → empty object, which surfaces as "no capability declared".
        let caps = value
            .get("capabilities")
            .cloned()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| Value::Object(Default::default()));
        *self.server_capabilities.lock().unwrap() = caps;
        // MCP wants a `notifications/initialized` from the client after initialize.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// `result.capabilities` from the most recent successful `initialize`; empty object until then.
    pub fn server_capabilities(&self) -> Value {
        self.server_capabilities.lock().unwrap().clone()
    }

    /// Did the plugin opt into the `neige.*` namespace? Only an exact `version` match counts; anything else warns and is treated as absent.
    pub fn has_kernel_callbacks_capability(&self, plugin_id: &str) -> bool {
        let caps = self.server_capabilities.lock().unwrap();
        let entry = caps.pointer(&format!(
            "/experimental/{}",
            KERNEL_CALLBACKS_CAPABILITY.replace('/', "~1")
        ));
        match entry {
            None => false,
            Some(node) => {
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

    /// MCP `tools/call`. `track_id` rides in `params._meta` under [`TRACK_META_KEY`] rather than in
    /// `arguments`: the tool's `input_schema` is plugin-authored and often `additionalProperties: false`.
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
        track_id: Option<&str>,
    ) -> Result<CallToolResult, RpcError> {
        let mut params = json!({
            "name": name,
            "arguments": arguments,
        });
        if let Some(track_id) = track_id {
            params["_meta"] = json!({ TRACK_META_KEY: { "id": track_id } });
        }
        let raw = self.call("tools/call", params).await?;
        serde_json::from_value::<CallToolResult>(raw).map_err(|e| {
            RpcError::internal(format!(
                "tools/call: response did not parse as CallToolResult: {e}"
            ))
        })
    }

    /// MCP `resources/read`. `ui://<plugin>/<view>` resources are served by the kernel, not the plugin; this is the plugin-facing counterpart.
    pub async fn resources_read(&self, uri: &str) -> Result<ResourceContents, RpcError> {
        let params = json!({ "uri": uri });
        let raw = self.call("resources/read", params).await?;
        serde_json::from_value::<ResourceContents>(raw).map_err(|e| {
            RpcError::internal(format!(
                "resources/read: response did not parse as ResourceContents: {e}"
            ))
        })
    }

    /// Outbound call (kernel → plugin).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        // Fast-path: a closed transport fails here instead of registering a doomed responder.
        if let Some(reason) = self.closed.lock().await.clone() {
            return Err(RpcError::internal(format!(
                "mcp transport closed: {reason}"
            )));
        }

        let id = RequestId::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.responders.lock().unwrap().insert(id.clone(), tx);
        // The slot leaves the map with THIS future however it ends — including a `timeout` cancelling a hung call.
        let _slot = ResponderSlot {
            map: &self.responders,
            id: id.clone(),
        };

        let frame = build_request_frame(&id, method, &params);
        if self.out_tx.send(OutboundFrame(frame)).await.is_err() {
            return Err(RpcError::internal("mcp writer task gone"));
        }

        match rx.await {
            Ok(res) => res,
            Err(_) => Err(RpcError::internal("response channel dropped")),
        }
    }

    /// Number of kernel → plugin requests still waiting for a reply.
    #[cfg(test)]
    pub(crate) fn pending_responders(&self) -> usize {
        self.responders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
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

    /// Take the inbound-request channel; subsequent calls return `None`.
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
            // Flush per message: for plugin RPC, latency wins over throughput.
            if let Err(e) = write.flush().await {
                let reason = format!("flush failed: {e}");
                *closed.lock().await = Some(reason);
                return;
            }
        }
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
        // Kept monomorphized: a `dyn` reader trips dyn-compatibility (`read_line` returns `ReadLine<'_, Self>`).
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
                    // Blocking here is the intentional backpressure; the writer task is a separate channel so responses still flow.
                    if in_req_tx.send(req).await.is_err() {
                        // No consumer on the request channel: synthesize a MethodNotFound so the plugin doesn't hang.
                        let frame =
                            build_error_response_frame(&id, &RpcError::method_not_found(&method));
                        let _ = out_tx.send(OutboundFrame(frame)).await;
                        continue;
                    }
                    // A separate joiner so a stuck handler can't block the reader.
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
                    // Notifications are by specification lossy, so a silent drop on a full buffer is correct.
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

/// Decoded JSON-RPC frame.
#[derive(Debug)]
pub(crate) enum Frame {
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

pub(crate) fn parse_frame(s: &str) -> Result<Frame, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| format!("json parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "frame is not an object".to_string())?;

    // Missing `jsonrpc` is accepted for ergonomic stubs.
    let _jsonrpc = obj.get("jsonrpc");

    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(|v| v.as_str()).map(String::from);

    match (id, method) {
        (Some(id_v), Some(m)) => {
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
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            Ok(Frame::Notification { method: m, params })
        }
        (None, None) => Err("frame has neither id nor method".into()),
    }
}

pub(crate) fn build_request_frame(id: &RequestId, method: &str, params: &Value) -> Vec<u8> {
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

pub(crate) fn build_notification_frame(method: &str, params: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

pub(crate) fn build_ok_response_frame(id: &RequestId, result: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

pub(crate) fn build_error_response_frame(id: &RequestId, err: &RpcError) -> Vec<u8> {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err,
    }))
    .expect("static json shape is always serializable");
    s.push('\n');
    s.into_bytes()
}

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

        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        let result = client
            .tools_call("make_status_card", json!({ "x": 1 }), None)
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

    /// Two `tools/call`s cancelled by `timeout` must leave the responder map as empty as before them.
    #[tokio::test]
    async fn timed_out_calls_leave_no_responder() {
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
                let Some(id) = v.get("id").cloned() else {
                    continue;
                };
                if v.get("method").and_then(|m| m.as_str()) != Some("initialize") {
                    // Never reply: the request is swallowed on purpose.
                    continue;
                }
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": KERNEL_PROTOCOL_VERSION,
                        "serverInfo": { "name": "stub", "version": "0.0.0" },
                        "capabilities": {}
                    }
                });
                let mut s = serde_json::to_string(&reply).unwrap();
                s.push('\n');
                writer.write_all(s.as_bytes()).await.unwrap();
                writer.flush().await.unwrap();
            }
        });

        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert_eq!(client.pending_responders(), 0, "clean after initialize");

        for attempt in 0..2 {
            let outcome = tokio::time::timeout(
                Duration::from_millis(300),
                client.call("tools/call", json!({ "name": "hang", "arguments": {} })),
            )
            .await;
            assert!(
                outcome.is_err(),
                "attempt {attempt}: the call must time out"
            );
        }
        assert_eq!(
            client.pending_responders(),
            0,
            "a cancelled call must not leave its responder slot behind"
        );
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), plugin_task).await;
    }

    #[tokio::test]
    async fn resources_read_parses_contents_with_meta_ui() {
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

        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
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
        let (kernel, plugin) = tokio::io::duplex(8 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);

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
            }
        });

        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        let result = client.call("hello", json!({})).await.expect("call");
        assert_eq!(result["got"], "hello");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), plugin_task).await;
    }

    /// Spawn a stub plugin that replies to `initialize` with `init_result`, then echoes further requests as `{"echo": method}`.
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
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn initialize_rejects_mismatched_protocol_version() {
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": "2099-01-01",
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        match McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        {
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
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: { "version": KERNEL_CALLBACKS_CAPABILITY_VERSION }
                }
            }
        }));
        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert!(client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_present_with_wrong_version_is_false() {
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: { "version": 2 }
                }
            }
        }));
        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_present_without_version_field_is_false() {
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    KERNEL_CALLBACKS_CAPABILITY: {}
                }
            }
        }));
        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }

    #[tokio::test]
    async fn capability_entirely_absent_is_false() {
        let (k_r, k_w, task) = spawn_init_stub(json!({
            "protocolVersion": KERNEL_PROTOCOL_VERSION,
            "serverInfo": { "name": "stub", "version": "0.0.0" },
            "capabilities": {}
        }));
        let client = McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("connect");
        assert!(!client.has_kernel_callbacks_capability("test.plugin"));
        drop(client);
        let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
    }
}
