//! Programmatic client for a card's `codex app-server` connection: JSON-RPC 2.0 over WebSocket over a unix socket.
//! `permessage-deflate` MUST NOT be offered or the server rejects the handshake; raw JSON without the WS upgrade is silently dropped. All methods used are `[experimental]`, so `initialize` sends `experimentalApi = true`.
//! A reader task demultiplexes responses (per-id oneshot), server requests, and notifications (unbounded mpsc); only the params/results we use are typed, and unknown fields are tolerated.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

mod client_transport;
mod server_requests;
pub use server_requests::{
    DynamicToolCallParams, DynamicToolCallResponse, DynamicToolRequest, DynamicToolText,
    ServerRequestId,
};
#[cfg(test)]
mod server_request_tests;
use client_transport::{PendingRequest, TransportAbort};

#[cfg(feature = "fixtures")]
#[path = "dedicated_codex/client_bootstrap.rs"]
mod client_bootstrap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::error::{CalmError, Result};
use crate::planner_model::TurnModelSelection;

/// The host is irrelevant over a unix socket, but tungstenite requires a `Host` header.
const WS_URI: &str = "ws://localhost/";

/// Every RPC we call returns a short acknowledgement (turn completion arrives as a notification), so a tight bound never truncates a long turn.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound for `connect(2)` and the WebSocket upgrade: a peer whose accept loop is blocked lets `connect(2)` succeed (listen backlog) and then never sends the HTTP 101.
/// This handshake is purely local, so anything that has not answered in 10 s is wedged, not slow.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// The notification channel is unbounded on purpose: responses and notifications share one reader loop, so a blocking notification send would stall every in-flight RPC response.
// Dropping `turn/completed` is unacceptable, and the per-card consumer drains promptly.

/// `clientInfo` block for `initialize`. Required by the schema.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// All methods we call are `[experimental]`, so `experimentalApi` is always true.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeCapabilities {
    experimental_api: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    client_info: ClientInfo,
    capabilities: InitializeCapabilities,
}

/// `initialize` result; tolerates extra fields.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InitializeResult {
    pub user_agent: String,
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
}

/// A single `turn/start` / `turn/steer` input item.
/// codex's `UserInput` is `camelCase` while this enum is `rename_all = "lowercase"`: a variant added without its own `rename` would serialize as `"localimage"`, which codex rejects with no signal on our side.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InputItem {
    /// `{"type":"text","text":"…"}`.
    Text { text: String },
    /// `{"type":"localImage","path":"/abs/path"}` — codex reads the file itself (same mount namespace). `detail` is deliberately not sent.
    /// A read or decode failure on codex's side is silent (placeholder text, no error), so a successful `turn/start` is no evidence the image was seen.
    #[serde(rename = "localImage")]
    LocalImage { path: String },
}

#[derive(Clone)]
pub struct ThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub developer_instructions: Option<String>,
    pub config: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub enum ThreadPermissionSelection {
    LegacySandbox(String),
    NamedProfile(String),
}

#[derive(Clone)]
pub struct PermissionThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub permissions: ThreadPermissionSelection,
    pub developer_instructions: Option<String>,
    pub config: Option<Value>,
}

impl std::fmt::Debug for PermissionThreadStartParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionThreadStartParams")
            .field("cwd", &self.cwd)
            .field("approval_policy", &self.approval_policy)
            .field("permissions", &self.permissions)
            .field("developer_instructions", &self.developer_instructions)
            .field("config", &redact_thread_start_config(&self.config))
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileListResponse {
    pub data: Vec<PermissionProfileSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PermissionProfileSummary {
    pub id: String,
    pub allowed: bool,
    pub description: Option<String>,
}

#[cfg(test)]
#[path = "codex_appserver/dedicated_permission_tests.rs"]
mod dedicated_permission_tests;

impl std::fmt::Debug for ThreadStartParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadStartParams")
            .field("cwd", &self.cwd)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_mode", &self.sandbox_mode)
            .field("developer_instructions", &self.developer_instructions)
            .field("config", &redact_thread_start_config(&self.config))
            .finish()
    }
}

pub(crate) fn redact_thread_start_config(cfg: &Option<serde_json::Value>) -> serde_json::Value {
    let Some(cfg) = cfg else {
        return Value::Null;
    };
    let mut redacted = cfg.clone();
    if let Some(policy) = redacted
        .pointer_mut("/shell_environment_policy")
        .and_then(Value::as_object_mut)
    {
        for value in policy.values_mut() {
            if let Value::Object(map) = value {
                for value in map.values_mut() {
                    if value.is_string() {
                        *value = Value::String("[REDACTED]".into());
                    }
                }
            }
        }
    }
    if let Some(servers) = redacted
        .get_mut("mcp_servers")
        .and_then(Value::as_object_mut)
    {
        for server in servers.values_mut() {
            if let Some(map) = server.as_object_mut() {
                for key in ["env", "http_headers", "bearer_token", "env_http_headers"] {
                    if let Some(value) = map.get_mut(key) {
                        *value = json!("[REDACTED]");
                    }
                }
            }
        }
    }
    redacted
}

impl InputItem {
    /// Convenience constructor for the text variant.
    pub fn text(s: impl Into<String>) -> Self {
        InputItem::Text { text: s.into() }
    }

    /// Convenience constructor for the local-image variant.
    pub fn local_image(path: impl Into<String>) -> Self {
        InputItem::LocalImage { path: path.into() }
    }
}

/// `thread/start` / `thread/resume` result; only `thread.id` is ever read.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ThreadResult {
    /// Raw `thread` object from the server.
    pub thread: Value,
    /// Resolved model (e.g. `gpt-5.5`).
    pub model: String,
}

impl ThreadResult {
    /// The thread id (`thread.id`); `None` only if the server returned a shape without it.
    pub fn thread_id(&self) -> Option<&str> {
        self.thread.get("id").and_then(Value::as_str)
    }
}

/// `turn/start` result — `{ "turn": { "id": …, … } }`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TurnStartResult {
    /// Raw `turn` object; we expose its id via [`TurnStartResult::turn_id`].
    pub turn: Value,
}

impl TurnStartResult {
    /// Needed as `expectedTurnId` for `turn/steer` and as `turnId` for `turn/interrupt`.
    pub fn turn_id(&self) -> Option<&str> {
        self.turn.get("id").and_then(Value::as_str)
    }
}

/// `turn/steer` result — `{ "turnId": "…" }`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnSteerResult {
    pub turn_id: String,
}

// `thread/read` + `thread/loaded/list` responses: narrowed mirrors of upstream `app-server-protocol` v2, defining only the fields the death arbiter reads.

/// Upstream `ThreadStatus`: internally tagged on `type`, camelCase variants. The arbiter keys on `Active`; the other arms mean "no turn running".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    #[serde(rename_all = "camelCase")]
    Active {
        active_flags: Vec<ThreadActiveFlag>,
    },
}

/// Upstream `ThreadActiveFlag`: either flag on an `Active` thread means blocked on a human — never reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadActiveFlag {
    WaitingOnApproval,
    WaitingOnUserInput,
}

/// `thread/read` response, narrowed to the fields the arbiter needs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadReadResponse {
    pub thread: ThreadView,
}

/// Narrowed mirror of upstream `Thread`. Upstream `turns` is a non-optional `Vec` (empty when not requested); `Option` + `#[serde(default)]` lets both `[]` and an absent field parse, and the arbiter treats both as "no turns".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadView {
    pub status: ThreadStatus,
    #[serde(default)]
    pub turns: Option<Vec<TurnView>>,
}

/// Narrowed mirror of upstream `Turn`: `completedAt` is the only field the arbiter reads (`null` = died mid-turn).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnView {
    #[serde(default)]
    pub completed_at: Option<i64>,
}

/// `thread/loaded/list` response; only `data` is kept, the pagination cursor is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadLoadedListResponse {
    pub data: Vec<String>,
}

/// One page of `model/list`; `SharedCodexAppServer::model_list` drains the pages.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListPage {
    /// Left undecoded on purpose: the catalog versions independently of us, so one unreadable preset must not empty the page — the caller decodes entries one by one.
    pub data: Vec<Value>,
    /// `None` (or an empty string) means "no further pages".
    pub next_cursor: Option<String>,
}

/// One catalog entry, narrowed to the fields `GET /api/models` proxies.
/// `id` is the preset identifier and `model` the slug the model is invoked by; only `model` may ever reach `turn/start` or `cards.payload_json`. `id` travels outward for presentation and must never come back as a selection.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub supported_reasoning_efforts: Vec<CodexReasoningEffortOption>,
    /// Bare string, not a closed enum: codex's `ReasoningEffort` accepts any non-empty string, and a closed enum here would fail the whole catalog on a new effort.
    pub default_reasoning_effort: String,
    /// Which entry the picker highlights, NOT which model this installation follows (that comes from `config/read`).
    pub is_default: bool,
}

/// One selectable reasoning effort for a model; `description` is codex's copy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexReasoningEffortOption {
    /// Bare string for the same reason as [`CodexModel::default_reasoning_effort`].
    pub reasoning_effort: String,
    pub description: String,
}

/// `config/read` response, narrowed to `config`. The envelope is camelCase but the wrapped `Config` is snake_case: writing `modelReasoningEffort` here parses to `None` forever, silently.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadResponse {
    pub config: CodexConfig,
}

/// `account/read` narrowed to what "logged in" is decided from (#1817). `account` is decoded as
/// present-or-null only, so the account identity codex returns (email, plan) is never kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRead {
    #[serde(deserialize_with = "present_or_null")]
    account: bool,
    requires_openai_auth: bool,
}

impl AccountRead {
    /// Codex's own rule: an account is present, or this configuration needs no OpenAI auth.
    pub fn logged_in(&self) -> bool {
        self.account || !self.requires_openai_auth
    }

    #[cfg(feature = "fixtures")]
    pub fn for_test(account: bool, requires_openai_auth: bool) -> Self {
        Self {
            account,
            requires_openai_auth,
        }
    }
}

/// `true` for any non-null value, without keeping it.
fn present_or_null<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    Ok(Option::<serde::de::IgnoredAny>::deserialize(deserializer)?.is_some())
}

/// The layer-merged effective config, narrowed to the two keys the model picker needs; field names are snake_case verbatim. Both are genuinely optional on codex's side.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CodexConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_reasoning_effort: Option<String>,
}

/// A server→client notification; anything not modeled lands in [`Notification::Other`] so codex version drift never breaks the consumer.
#[derive(Debug, Clone)]
pub enum Notification {
    /// `thread/started` — a thread was created/loaded on this connection.
    ThreadStarted { params: Value },
    /// `thread/status/changed` — the raw status `Value` (`{ "type": "idle" | "active" | ... }`) plus the thread id.
    ThreadStatusChanged { thread_id: String, status: Value },
    /// `turn/started` — carries `threadId` + the full `turn` object.
    TurnStarted { thread_id: String, turn: Value },
    /// `turn/completed` — the terminal event of a turn.
    TurnCompleted { thread_id: String, turn: Value },
    /// Any `item/*` event; the exact method is preserved so a consumer can branch on it.
    Item { method: String, params: Value },
    /// Any method we don't model; `method` + `params` are preserved.
    Other { method: String, params: Value },
}

impl Notification {
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Notification::ThreadStarted { params } => {
                crate::shared_codex_appserver::thread_id_from_started(params)
            }
            Notification::ThreadStatusChanged { thread_id, .. }
            | Notification::TurnStarted { thread_id, .. }
            | Notification::TurnCompleted { thread_id, .. } => Some(thread_id.as_str()),
            Notification::Item { params, .. } | Notification::Other { params, .. } => {
                crate::shared_codex_appserver::other_thread_id(params)
            }
        }
    }

    /// Never fails: unknown / malformed shapes degrade to [`Notification::Other`].
    fn parse(method: String, params: Value) -> Self {
        let thread_id = |p: &Value| {
            p.get("threadId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match method.as_str() {
            "thread/started" => Notification::ThreadStarted { params },
            "thread/status/changed" => Notification::ThreadStatusChanged {
                thread_id: thread_id(&params),
                status: params.get("status").cloned().unwrap_or(Value::Null),
            },
            "turn/started" => Notification::TurnStarted {
                thread_id: thread_id(&params),
                turn: params.get("turn").cloned().unwrap_or(Value::Null),
            },
            "turn/completed" => Notification::TurnCompleted {
                thread_id: thread_id(&params),
                turn: params.get("turn").cloned().unwrap_or(Value::Null),
            },
            m if m.starts_with("item/") => Notification::Item { method, params },
            _ => Notification::Other { method, params },
        }
    }
}

/// The receiving half of the notification stream; the channel ends when the connection closes.
pub struct NotificationStream {
    rx: mpsc::UnboundedReceiver<Notification>,
}

impl NotificationStream {
    /// Await the next notification, or `None` once the connection closed.
    pub async fn recv(&mut self) -> Option<Notification> {
        self.rx.recv().await
    }

    /// Like `recv`, but a closed stream is a deterministic error.
    pub async fn recv_result(&mut self) -> Result<Notification> {
        self.rx.recv().await.ok_or_else(notification_stream_closed)
    }

    /// Await the next notification satisfying `predicate`; non-matching ones are consumed, and a stream that ends first is an error rather than `None`.
    pub async fn await_notification(
        &mut self,
        mut predicate: impl FnMut(&Notification) -> bool,
    ) -> Result<Notification> {
        loop {
            let notification = self.recv_result().await?;
            if predicate(&notification) {
                return Ok(notification);
            }
        }
    }
}

fn notification_stream_closed() -> CalmError {
    CalmError::CodexAppServer(
        "notification stream closed (server EOF, WS read error, or reader task exit)".to_string(),
    )
}

/// In-flight request registry: JSON-RPC id -> sender for its response.
type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;

/// A JSON-RPC error object as returned by the server (`-32600` etc.).
#[derive(Debug, Clone, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// The write half, behind a `Mutex` so concurrent requests cannot interleave WS frames.
type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocketStream<UnixStream>, Message>>>;

/// An async client for one card's `codex app-server` over WebSocket-over-UDS; `&self` methods may run concurrently, but there is a single handle.
pub struct CodexAppServer {
    sink: WsSink,
    transport: Arc<TransportAbort>,
    server_requests: Arc<server_requests::Registration>,
    pending: Pending,
    next_id: AtomicU64,
    /// A leak/wedge backstop for a request whose response never arrives; lifecycle is driven by notifications / EOF / child exit, not by this timer.
    request_timeout: Duration,
    /// Kept so dropping the client aborts the reader task.
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        self.server_requests.close();
        self.transport.poison();
        self.reader.abort();
    }
}

/// The message a [`CONNECT_TIMEOUT`] expiry produces: names the stage, the socket, the budget and the observable peer state.
async fn connect_timeout_diagnostic(sock_path: &Path, awaited: &str, peer_state: &str) -> String {
    let sock_exists = sock_path.exists();
    // A fresh probe: ECONNREFUSED means a stale socket file, success means a listener is still bound — opposite repairs. Bounded and async on purpose: a blocking probe would itself hang against a full listen backlog.
    let listener_bound = match tokio::time::timeout(
        Duration::from_millis(200),
        UnixStream::connect(sock_path),
    )
    .await
    {
        Ok(Ok(_)) => Some(true),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => Some(false),
        // Backlog-full, permission errors, our own 200 ms expiry: inconclusive.
        Ok(Err(_)) | Err(_) => None,
    };
    let listener = match listener_bound {
        Some(true) => "a listener is still bound",
        Some(false) => "nothing is listening (stale socket file)",
        None => "listener state inconclusive",
    };
    format!(
        "timed out after {}s waiting for {awaited} on {} — peer state: \
         socket file {}, {listener}; {peer_state}",
        CONNECT_TIMEOUT.as_secs(),
        sock_path.display(),
        if sock_exists { "present" } else { "GONE" },
    )
}

/// Build the `turn/start` params frame, split out so the frame can be asserted on without a daemon.
/// A `None` in `selection` omits the key entirely, never `null`: codex's overrides are sticky, and an omitted key means "leave the thread's current override alone".
/// `effort` is spelled `effort` (not `reasoningEffort`); `clientUserMessageId` is echoed back by codex as `item.clientId` on the `userMessage` item, the kernel's only key for matching that echo.
fn turn_start_params(
    thread_id: &str,
    input: &[InputItem],
    selection: &TurnModelSelection,
    client_user_message_id: Option<&str>,
) -> Value {
    let mut params = json!({ "threadId": thread_id, "input": input });
    let map = params
        .as_object_mut()
        .expect("a json! object literal is an object");
    if let Some(model) = selection.model.as_deref() {
        map.insert("model".into(), Value::String(model.to_string()));
    }
    if let Some(effort) = selection.effort.as_deref() {
        map.insert("effort".into(), Value::String(effort.to_string()));
    }
    if let Some(client_id) = client_user_message_id {
        map.insert(
            "clientUserMessageId".into(),
            Value::String(client_id.to_string()),
        );
    }
    params
}

/// The `turn/steer` frame: three required keys plus the optional `clientUserMessageId` (omitted, never `null`). No model and no effort: codex rejects settings on a steer.
fn turn_steer_params(
    thread_id: &str,
    expected_turn_id: &str,
    input: &[InputItem],
    client_user_message_id: Option<&str>,
) -> Value {
    let mut params = json!({
        "threadId": thread_id,
        "expectedTurnId": expected_turn_id,
        "input": input,
    });
    if let Some(client_id) = client_user_message_id {
        params
            .as_object_mut()
            .expect("a json! object literal is an object")
            .insert(
                "clientUserMessageId".into(),
                Value::String(client_id.to_string()),
            );
    }
    params
}

impl CodexAppServer {
    /// Register the sole dynamic-tool consumer for this connection; the default is explicit refusal.
    pub fn take_dynamic_tool_requests(&self) -> Result<mpsc::Receiver<DynamicToolRequest>> {
        self.server_requests.take()
    }

    /// Test-only: a fully-constructed [`CodexAppServer`] over an in-process `UnixStream::pair` handshake; the returned server end must be kept alive or the connection closes.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) async fn connect_pair_for_test()
    -> (Self, NotificationStream, WebSocketStream<UnixStream>) {
        let (client_io, server_io) = UnixStream::pair().expect("unix socket pair");
        let req = WS_URI.into_client_request().unwrap();
        let client_fut = tokio_tungstenite::client_async(req, client_io);
        let server_fut = tokio_tungstenite::accept_async(server_io);
        let (client_res, server_res) = tokio::join!(client_fut, server_fut);
        let (client_ws, _resp) = client_res.expect("client handshake");
        let server = server_res.expect("server handshake");

        let transport =
            Arc::new(TransportAbort::new(client_ws.get_ref()).expect("retain owned socket"));
        let (write, read) = client_ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let server_requests = Arc::new(server_requests::Registration::default());
        let reader = tokio::spawn(reader_loop(
            read,
            pending.clone(),
            notif_tx,
            sink.clone(),
            transport.clone(),
            server_requests.clone(),
        ));
        let client = Self {
            sink,
            transport,
            server_requests,
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reader,
        };
        (client, NotificationStream { rx: notif_rx }, server)
    }

    /// Connect to a `codex app-server` on `sock_path`, spawn the reader, return the client and its [`NotificationStream`]. Does NOT send `initialize`.
    pub async fn connect(sock_path: impl AsRef<Path>) -> Result<(Self, NotificationStream)> {
        let sock_path = sock_path.as_ref();
        let stream =
            match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(sock_path)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => {
                    return Err(CalmError::CodexAppServer(format!(
                        "connect unix socket {}: {e}",
                        sock_path.display()
                    )));
                }
                Err(_) => {
                    return Err(CalmError::CodexAppServer(
                        connect_timeout_diagnostic(
                            sock_path,
                            "connect(2) on the unix socket",
                            "the socket file exists but the peer never completed the connection \
                         (its listen backlog is full, i.e. its accept loop is not running)",
                        )
                        .await,
                    ));
                }
            };

        // `IntoClientRequest` on a `&str` URI adds NO `Sec-WebSocket-Extensions`, so permessage-deflate is never offered.
        let request = WS_URI
            .into_client_request()
            .map_err(|e| CalmError::CodexAppServer(format!("build ws handshake request: {e}")))?;

        let (ws, _resp) = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::client_async(request, stream),
        )
        .await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Err(CalmError::CodexAppServer(format!(
                    "ws handshake over {}: {e}",
                    sock_path.display()
                )));
            }
            Err(_) => {
                return Err(CalmError::CodexAppServer(
                    connect_timeout_diagnostic(
                        sock_path,
                        "the WebSocket upgrade response (HTTP 101) after connect(2) succeeded",
                        "the peer accepted the connection and then went silent — it is \
                             wedged, or its accept loop is head-of-line blocked serving another \
                             connection",
                    )
                    .await,
                ));
            }
        };

        let transport =
            Arc::new(TransportAbort::new(ws.get_ref()).map_err(|error| {
                CalmError::CodexAppServer(format!("retain owned socket: {error}"))
            })?);
        let (write, read) = ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        // Unbounded: notification delivery must never block the reader's response routing.
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        let server_requests = Arc::new(server_requests::Registration::default());
        let reader = tokio::spawn(reader_loop(
            read,
            pending.clone(),
            notif_tx,
            sink.clone(),
            transport.clone(),
            server_requests.clone(),
        ));

        tracing::debug!(sock = %sock_path.display(), "codex app-server: connected");

        let client = Self {
            sink,
            transport,
            server_requests,
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reader,
        };
        Ok((client, NotificationStream { rx: notif_rx }))
    }

    /// Override the per-request response timeout, builder-style.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// The current per-request response timeout.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Must be the first call on a fresh connection.
    pub async fn initialize(&self, client_info: ClientInfo) -> Result<InitializeResult> {
        let params = InitializeParams {
            client_info,
            capabilities: InitializeCapabilities {
                experimental_api: true,
            },
        };
        self.request("initialize", json!(params)).await
    }

    /// `thread/start` — a brand-new thread has NO rollout on disk until a turn runs, so a second connection cannot `thread/resume` it until then.
    pub async fn thread_start(&self, developer_instructions: Option<&str>) -> Result<ThreadResult> {
        let params = match developer_instructions {
            Some(prompt) => json!({ "developerInstructions": prompt }),
            None => json!({}),
        };
        self.request("thread/start", params).await
    }

    pub async fn thread_start_with_params(
        &self,
        params: ThreadStartParams,
    ) -> Result<ThreadResult> {
        self.thread_start_with_permissions(PermissionThreadStartParams {
            cwd: params.cwd,
            approval_policy: params.approval_policy,
            permissions: ThreadPermissionSelection::LegacySandbox(params.sandbox_mode),
            developer_instructions: params.developer_instructions,
            config: params.config,
        })
        .await
    }

    pub async fn thread_start_with_permissions(
        &self,
        params: PermissionThreadStartParams,
    ) -> Result<ThreadResult> {
        self.thread_start_with_permissions_and_tools(params, Vec::new())
            .await
    }

    pub(crate) async fn thread_start_with_params_and_tools(
        &self,
        params: ThreadStartParams,
        tools: Vec<Value>,
    ) -> Result<ThreadResult> {
        self.thread_start_with_permissions_and_tools(
            PermissionThreadStartParams {
                cwd: params.cwd,
                approval_policy: params.approval_policy,
                permissions: ThreadPermissionSelection::LegacySandbox(params.sandbox_mode),
                developer_instructions: params.developer_instructions,
                config: params.config,
            },
            tools,
        )
        .await
    }

    async fn thread_start_with_permissions_and_tools(
        &self,
        params: PermissionThreadStartParams,
        tools: Vec<Value>,
    ) -> Result<ThreadResult> {
        let mut value = json!({"cwd":params.cwd,"approvalPolicy":params.approval_policy});
        if !tools.is_empty() {
            value["dynamicTools"] = json!(tools);
        }
        match params.permissions {
            ThreadPermissionSelection::LegacySandbox(mode) => {
                value["sandbox"] = Value::String(mode);
            }
            ThreadPermissionSelection::NamedProfile(profile) => {
                if profile.trim().is_empty() {
                    return Err(CalmError::BadRequest(
                        "named permission profile required".into(),
                    ));
                }
                value["permissions"] = Value::String(profile);
            }
        }
        if let Some(prompt) = params.developer_instructions {
            value["developerInstructions"] = Value::String(prompt);
        }
        if let Some(config) = params.config {
            value["config"] = config;
        }
        self.request("thread/start", value).await
    }

    pub async fn permission_profile_list(
        &self,
        cwd: &str,
        cursor: Option<&str>,
    ) -> Result<PermissionProfileListResponse> {
        let mut value = json!({"cwd":cwd});
        if let Some(cursor) = cursor {
            value["cursor"] = json!(cursor);
        }
        self.request("permissionProfile/list", value).await
    }

    pub async fn thread_resume_named(
        &self,
        thread_id: &str,
        profile: &str,
    ) -> Result<ThreadResult> {
        if profile.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "named permission profile required".into(),
            ));
        }
        self.request(
            "thread/resume",
            json!({"threadId":thread_id,"permissions":profile,"approvalPolicy":"never"}),
        )
        .await
    }

    /// Full provider-owned details for exact-thread reconciliation; no new recorder.
    pub async fn thread_read_full(&self, thread_id: &str) -> Result<ThreadResult> {
        self.request(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":true}),
        )
        .await
    }

    /// `thread/resume` — fails with `-32600 "no rollout found …"` if the thread has not yet run a turn.
    pub async fn thread_resume(&self, thread_id: &str) -> Result<ThreadResult> {
        self.request("thread/resume", json!({ "threadId": thread_id }))
            .await
    }

    pub async fn thread_resume_with_config(
        &self,
        thread_id: &str,
        config: Option<serde_json::Value>,
    ) -> Result<ThreadResult> {
        let mut value = json!({ "threadId": thread_id });
        if let Some(config) = config {
            value["config"] = config;
        }
        self.request("thread/resume", value).await
    }

    /// `thread/read` — current status and, with `include_turns`, the turn history whose last `completed_at` is the died-mid-turn discriminator.
    pub async fn thread_read(
        &self,
        thread_id: &str,
        include_turns: bool,
    ) -> Result<ThreadReadResponse> {
        self.request(
            "thread/read",
            json!({ "threadId": thread_id, "includeTurns": include_turns }),
        )
        .await
    }

    /// `thread/loaded/list` — the thread ids currently loaded in daemon memory (pagination cursor dropped).
    pub async fn thread_loaded_list(&self) -> Result<Vec<String>> {
        let resp: ThreadLoadedListResponse = self.request("thread/loaded/list", json!({})).await?;
        Ok(resp.data)
    }

    /// `turn/start` — returns the turn id quickly; the work streams as notifications. `selection` is required so every caller answers the model question out loud (`TurnModelSelection::inherit` for nothing to say).
    pub async fn turn_start(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
    ) -> Result<TurnStartResult> {
        self.turn_start_with_client_id(thread_id, input, selection, None)
            .await
    }

    /// `turn/start` carrying `clientUserMessageId`; the planner drain is the one caller with an id to send.
    pub async fn turn_start_with_client_id(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnStartResult> {
        self.request(
            "turn/start",
            turn_start_params(thread_id, &input, selection, client_user_message_id),
        )
        .await
    }

    /// `model/list` — one page. `includeHidden` is pinned to `false`: the picker-visibility filter is codex's.
    pub async fn model_list(
        &self,
        cursor: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ModelListPage> {
        let mut params = json!({ "includeHidden": false });
        if let Some(cursor) = cursor {
            params["cursor"] = Value::String(cursor.to_string());
        }
        self.request_until("model/list", params, deadline).await
    }

    /// `config/read` — `cwd` decides which project layers are folded in; a `None` read must not be presented as a thread's effective default.
    pub async fn config_read(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ConfigReadResponse> {
        let mut params = json!({ "includeLayers": false });
        if let Some(cwd) = cwd {
            params["cwd"] = Value::String(cwd.to_string());
        }
        self.request_until("config/read", params, deadline).await
    }

    /// `account/read` without a token refresh — whether this daemon is logged in.
    pub async fn account_read(&self, deadline: tokio::time::Instant) -> Result<AccountRead> {
        self.request_until("account/read", json!({}), deadline)
            .await
    }

    /// `turn/steer` — push more input into the running turn. Codex refuses with `-32600` when no turn or a different turn is active (reaches the caller as [`CalmError::CodexRefused`]).
    /// `client_user_message_id` is echoed back as `item.clientId` on the steered `userMessage` item.
    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnSteerResult> {
        self.request(
            "turn/steer",
            turn_steer_params(thread_id, expected_turn_id, &input, client_user_message_id),
        )
        .await
    }

    /// `thread/inject_items` — push context items without starting a turn; inject alone creates no rollout, so it does not make a turn-less thread resumable.
    pub async fn inject_items(&self, thread_id: &str, items: Vec<Value>) -> Result<()> {
        let _: Value = self
            .request(
                "thread/inject_items",
                json!({ "threadId": thread_id, "items": items }),
            )
            .await?;
        Ok(())
    }

    /// `turn/interrupt` — cancel a running turn.
    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        let response: Result<Value> = self
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await;
        match response {
            Ok(_) => Ok(()),
            // A turn that completed between our snapshot and the daemon handling this is already in the requested state; codex reports that race as this exact response.
            Err(CalmError::CodexRefused(message))
                if message
                    == "turn/interrupt failed: no active turn to interrupt (code -32600)" =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Core round-trip. A JSON-RPC `error` frame maps to [`CalmError::CodexRefused`]; transport failures and a dead reader map to [`CalmError::CodexAppServer`].
    async fn request<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T> {
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        self.request_until(method, params, deadline).await
    }

    /// [`Self::request`] with an explicit absolute deadline. Cancellation removes the pending entry; if it interrupts an incomplete send the owned socket is shut down unflushed and the outcome stays unknown.
    async fn request_until<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
    ) -> Result<T> {
        // An already-spent budget is answered without touching the wire: writing the frame would put a request on the daemon whose answer we already decided to ignore.
        if tokio::time::Instant::now() >= deadline {
            return Err(CalmError::CodexAppServer(format!(
                "request {method} skipped: the caller's budget was already spent"
            )));
        }

        self.transport.check()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _pending = PendingRequest::new(self.pending.clone(), id);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let text = serde_json::to_string(&frame)?;

        // Write under the sink lock so concurrent requests don't interleave WS frames.
        {
            let mut sink = self.sink.lock().await;
            // Created after the lock: cancellation closes the owned socket BEFORE releasing the sink.
            let mut sending = self.transport.sending()?;
            if let Err(e) = sink.send(Message::Text(text)).await {
                // Drop the now-unanswerable pending entry.
                self.pending.lock().unwrap().remove(&id);
                return Err(CalmError::CodexAppServer(format!("send {method}: {e}")));
            }
            sending.complete();
        }

        tracing::trace!(id, method, "codex app-server: request sent");

        // On elapse remove our own pending entry so the map does not leak the never-fired oneshot. This timer is not a turn lifecycle criterion.
        let outcome = match tokio::time::timeout_at(deadline, rx).await {
            Ok(received) => received,
            Err(_elapsed) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(CalmError::CodexAppServer(format!(
                    "request {method} timed out"
                )));
            }
        };

        match outcome {
            Ok(Ok(value)) => serde_json::from_value(value)
                .map_err(|e| CalmError::CodexAppServer(format!("decode {method} result: {e}"))),
            // The one place codex's own refusal is still distinguishable from everything else that can go wrong.
            Ok(Err(rpc)) => Err(CalmError::CodexRefused(format!(
                "{method} failed: {} (code {})",
                rpc.message, rpc.code
            ))),
            // Sender dropped without sending: the reader task ended before our response arrived.
            Err(_) => Err(CalmError::CodexAppServer(format!(
                "{method}: connection closed before response"
            ))),
        }
    }
}

/// Background reader: demultiplexes inbound frames into responses, server requests and notifications; exits on close, transport error, or when the notification consumer is gone.
async fn reader_loop(
    mut read: futures_util::stream::SplitStream<WebSocketStream<UnixStream>>,
    pending: Pending,
    notif_tx: mpsc::UnboundedSender<Notification>,
    sink: WsSink,
    transport: Arc<TransportAbort>,
    registration: Arc<server_requests::Registration>,
) {
    let mut requests = server_requests::Dispatch::new(sink, transport, registration);
    loop {
        let frame = tokio::select! {
            finished = requests.tasks.join_next() => {
                if matches!(finished, Some(Ok(true))) { continue; }
                break;
            }
            frame = read.next() => match frame { Some(frame) => frame, None => break },
        };
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "codex app-server: ws read error; reader stopping");
                break;
            }
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            // tungstenite auto-replies to pings; a Close ends the stream on the next poll.
            Message::Close(_) => {
                tracing::debug!("codex app-server: ws close frame; reader stopping");
                break;
            }
            _ => continue,
        };

        let obj: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, frame = %text, "codex app-server: undecodable frame; skipping");
                continue;
            }
        };

        // Bidirectional IDs are independent: a server request must never consume a pending client RPC with the same ID.
        if obj.get("method").is_some() && obj.get("id").is_some() {
            if !requests.accept(&obj) {
                break;
            }
            continue;
        }
        // We emit integer IDs, accepting string-encoded integers in responses.
        if let Some(id) = obj.get("id").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        }) {
            let sender = { pending.lock().unwrap().remove(&id) };
            if let Some(sender) = sender {
                let payload = if let Some(err) = obj.get("error") {
                    match serde_json::from_value::<RpcError>(err.clone()) {
                        Ok(rpc) => Err(rpc),
                        Err(_) => Err(RpcError {
                            code: 0,
                            message: format!("malformed error frame: {err}"),
                        }),
                    }
                } else {
                    Ok(obj.get("result").cloned().unwrap_or(Value::Null))
                };
                // Receiver may be gone if the request future was dropped.
                let _ = sender.send(payload);
                continue;
            }
            // Untracked id — treat as a notification if it carries a method, else drop.
        }

        if let Some(method) = obj.get("method").and_then(Value::as_str) {
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            let notif = Notification::parse(method.to_string(), params);
            // `unbounded_send` never awaits capacity, so a slow consumer can never block response routing; the only failure is a dropped receiver.
            if notif_tx.send(notif).is_err() {
                tracing::debug!("codex app-server: notification consumer dropped; reader stopping");
                break;
            }
        }
    }

    // Drain pending requests so their futures resolve with "connection closed" instead of hanging.
    let mut guard = pending.lock().unwrap();
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_read_parses_last_turn_completed_at_null() {
        // (a) died-mid-turn: last turn `completedAt: null`.
        let resp: ThreadReadResponse = serde_json::from_value(json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [
                    { "completedAt": 1700 },
                    { "completedAt": null }
                ]
            }
        }))
        .unwrap();
        assert_eq!(resp.thread.status, ThreadStatus::Idle);
        let turns = resp.thread.turns.unwrap();
        assert_eq!(turns.last().unwrap().completed_at, None);
    }

    #[test]
    fn thread_read_parses_last_turn_completed_at_some() {
        // (b) clean finish / deliberate abort: last turn `completedAt: <ts>`.
        let resp: ThreadReadResponse = serde_json::from_value(json!({
            "thread": {
                "status": { "type": "idle" },
                "turns": [ { "completedAt": 1700 }, { "completedAt": 1800 } ]
            }
        }))
        .unwrap();
        assert_eq!(
            resp.thread.turns.unwrap().last().unwrap().completed_at,
            Some(1800)
        );
    }

    #[test]
    fn thread_read_parses_active_waiting_on_user_input() {
        // (c) status `active` with `activeFlags:["waitingOnUserInput"]`.
        let resp: ThreadReadResponse = serde_json::from_value(json!({
            "thread": {
                "status": {
                    "type": "active",
                    "activeFlags": ["waitingOnUserInput"]
                },
                "turns": []
            }
        }))
        .unwrap();
        assert_eq!(
            resp.thread.status,
            ThreadStatus::Active {
                active_flags: vec![ThreadActiveFlag::WaitingOnUserInput]
            }
        );
        // empty list deserializes to Some([]) — "no turns" for the arbiter.
        assert_eq!(resp.thread.turns, Some(vec![]));
    }

    #[test]
    fn thread_read_parses_not_loaded_and_absent_turns() {
        // (d) `notLoaded`, and `turns` absent (include_turns=false) → None.
        let resp: ThreadReadResponse = serde_json::from_value(json!({
            "thread": { "status": { "type": "notLoaded" } }
        }))
        .unwrap();
        assert_eq!(resp.thread.status, ThreadStatus::NotLoaded);
        assert_eq!(resp.thread.turns, None);
    }

    #[test]
    fn thread_loaded_list_plucks_data_and_tolerates_cursor() {
        let resp: ThreadLoadedListResponse = serde_json::from_value(json!({
            "data": ["t-1", "t-2"],
            "nextCursor": "opaque"
        }))
        .unwrap();
        assert_eq!(resp.data, vec!["t-1".to_string(), "t-2".to_string()]);
    }

    /// A real WS-over-UnixStream connection: a fully-constructed client on one end and a raw [`WebSocketStream`] the test drives on the other, no `codex` binary.
    struct Harness {
        client: CodexAppServer,
        /// Must stay alive even when not drained: dropping it closes the channel and stops the reader.
        _notifs: NotificationStream,
        /// Server-side WS end; the test reads requests off it and writes responses back.
        server: WebSocketStream<UnixStream>,
    }

    async fn harness() -> Harness {
        let (client_io, server_io) = UnixStream::pair().expect("unix socket pair");

        // Drive both handshakes concurrently.
        let req = WS_URI.into_client_request().unwrap();
        let client_fut = tokio_tungstenite::client_async(req, client_io);
        let server_fut = tokio_tungstenite::accept_async(server_io);
        let (client_res, server_res) = tokio::join!(client_fut, server_fut);
        let (client_ws, _resp) = client_res.expect("client handshake");
        let server = server_res.expect("server handshake");

        let transport =
            Arc::new(TransportAbort::new(client_ws.get_ref()).expect("retain owned socket"));
        let (write, read) = client_ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let server_requests = Arc::new(server_requests::Registration::default());
        let reader = tokio::spawn(reader_loop(
            read,
            pending.clone(),
            notif_tx,
            sink.clone(),
            transport.clone(),
            server_requests.clone(),
        ));

        let client = CodexAppServer {
            sink,
            transport,
            server_requests,
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reader,
        };
        Harness {
            client,
            _notifs: NotificationStream { rx: notif_rx },
            server,
        }
    }

    /// Pull the next text frame off the server end as parsed JSON.
    async fn server_recv_json(server: &mut WebSocketStream<UnixStream>) -> Value {
        loop {
            match server.next().await.expect("frame").expect("ws ok") {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Close(_) => panic!("server saw close before a request"),
                _ => continue,
            }
        }
    }

    async fn server_send_json(server: &mut WebSocketStream<UnixStream>, v: Value) {
        server
            .send(Message::Text(serde_json::to_string(&v).unwrap()))
            .await
            .expect("server send");
    }

    #[tokio::test]
    async fn recv_result_returns_err_when_server_closes() {
        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        server.close(None).await.expect("server close");

        let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
            .await
            .expect("recv_result should resolve when the server closes")
            .expect_err("closed notification stream must be an error");
        assert!(
            matches!(err, CalmError::CodexAppServer(msg) if msg.contains("notification stream closed"))
        );
    }

    #[tokio::test]
    async fn recv_result_returns_err_after_reader_task_exits() {
        let (client, mut notifs, _server) = CodexAppServer::connect_pair_for_test().await;
        drop(client);

        let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
            .await
            .expect("recv_result should resolve after reader task exit")
            .expect_err("reader task exit must close the notification stream");
        assert!(
            matches!(err, CalmError::CodexAppServer(msg) if msg.contains("notification stream closed"))
        );

        let err = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
            .await
            .expect("recv_result should stay resolved after reader task exit")
            .expect_err("subsequent recv_result calls must also error");
        assert!(
            matches!(err, CalmError::CodexAppServer(msg) if msg.contains("notification stream closed"))
        );
    }

    #[tokio::test]
    async fn malformed_notification_frame_is_skipped() {
        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        server
            .send(Message::Text("{not valid json".to_string()))
            .await
            .expect("server send malformed frame");
        server_send_json(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "method": "item/agentMessage/delta",
                "params": { "delta": "after-malformed" },
            }),
        )
        .await;

        let notification = tokio::time::timeout(Duration::from_secs(1), notifs.recv_result())
            .await
            .expect("recv_result should skip malformed frames and reach the next notification")
            .expect("valid notification after malformed frame should be delivered");
        match notification {
            Notification::Item { method, params } => {
                assert_eq!(method, "item/agentMessage/delta");
                assert_eq!(
                    params.get("delta").and_then(Value::as_str),
                    Some("after-malformed")
                );
            }
            other => panic!("expected Item notification, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn await_notification_returns_err_when_closed_without_match() {
        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        server_send_json(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "method": "thread/status/changed",
                "params": { "threadId": "t1", "status": { "type": "active" } },
            }),
        )
        .await;
        server.close(None).await.expect("server close");

        let err = tokio::time::timeout(
            Duration::from_secs(1),
            notifs.await_notification(|notification| {
                matches!(notification, Notification::TurnCompleted { .. })
            }),
        )
        .await
        .expect("await_notification should resolve when the stream closes")
        .expect_err("closed stream before a predicate match must be an error");
        assert!(
            matches!(err, CalmError::CodexAppServer(msg) if msg.contains("notification stream closed"))
        );
    }

    #[tokio::test]
    async fn notification_await_apis_return_matching_notifications() {
        let (_client, mut notifs, mut server) = CodexAppServer::connect_pair_for_test().await;
        server_send_json(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "method": "turn/completed",
                "params": { "threadId": "t1", "turn": { "id": "turn-1" } },
            }),
        )
        .await;

        let notification = notifs.recv_result().await.expect("notification");
        match notification {
            Notification::TurnCompleted { thread_id, turn } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(turn.get("id").and_then(Value::as_str), Some("turn-1"));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }

        server_send_json(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "method": "item/agentMessage/delta",
                "params": { "delta": "ignored" },
            }),
        )
        .await;
        server_send_json(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "method": "turn/completed",
                "params": { "threadId": "t2", "turn": { "id": "turn-2" } },
            }),
        )
        .await;

        let notification = notifs
            .await_notification(|notification| {
                matches!(
                    notification,
                    Notification::TurnCompleted { thread_id, .. } if thread_id == "t2"
                )
            })
            .await
            .expect("matching notification");
        match notification {
            Notification::TurnCompleted { thread_id, turn } => {
                assert_eq!(thread_id, "t2");
                assert_eq!(turn.get("id").and_then(Value::as_str), Some("turn-2"));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn default_request_timeout_is_30s() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn with_request_timeout_overrides_default() {
        // Stand up only enough to construct a client (no real IO needed).
        let h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_millis(5));
        assert_eq!(client.request_timeout(), Duration::from_millis(5));
    }

    #[test]
    fn thread_start_params_debug_scrubs_neige_mcp_token() {
        let params = ThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: Some(json!({
                "shell_environment_policy": {
                    "set": {
                        "NEIGE_MCP_SOCKET": "/tmp/x.sock",
                        "NEIGE_MCP_TOKEN": "secret-abcdef",
                    },
                    "append": {
                        "SOME_KEY": "some_value",
                    }
                }
            })),
        };

        let rendered = format!("{params:?}");
        assert!(!rendered.contains("secret-abcdef"));
        assert!(!rendered.contains("some_value"));
        assert!(rendered.contains("\"[REDACTED]\""));
    }

    #[test]
    fn thread_start_config_redactor_preserves_inherit_key_names() {
        let redacted = redact_thread_start_config(&Some(json!({
            "shell_environment_policy": {
                "set": {
                    "NEIGE_MCP_TOKEN": "secret-abcdef",
                },
                "inherit": ["KEEP_ME"],
            }
        })));

        assert_eq!(
            redacted.pointer("/shell_environment_policy/inherit"),
            Some(&json!(["KEEP_ME"]))
        );
        assert_eq!(
            redacted.pointer("/shell_environment_policy/set/NEIGE_MCP_TOKEN"),
            Some(&json!("[REDACTED]"))
        );
    }

    #[tokio::test]
    async fn thread_start_sends_developer_instructions_when_present() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.thread_start(Some("role prompt"));

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/start")
            );
            assert_eq!(
                req.get("params")
                    .and_then(|params| params.get("developerInstructions"))
                    .and_then(Value::as_str),
                Some("role prompt")
            );

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "with-prompt" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });
        assert_eq!(req_fut.await.unwrap().thread_id(), Some("with-prompt"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_start_omits_developer_instructions_when_absent() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.thread_start(None);

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/start")
            );
            assert!(
                !req.get("params")
                    .and_then(Value::as_object)
                    .is_some_and(|params| params.contains_key("developerInstructions")),
                "developerInstructions must be omitted when absent; got: {req}"
            );

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "without-prompt" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });
        assert_eq!(req_fut.await.unwrap().thread_id(), Some("without-prompt"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn turn_interrupt_treats_an_already_finished_turn_as_success() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let interrupt = client.turn_interrupt("thread-1", "turn-1");

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("turn/interrupt")
            );
            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32600, "message": "no active turn to interrupt" },
                }),
            )
            .await;
            h.server
        });

        interrupt
            .await
            .expect("interrupt is idempotent when the named turn already finished");
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn turn_interrupt_propagates_other_rpc_errors() {
        let cases = [
            (-32600, "expected active turn id turn-1 but found turn-2"),
            (-32601, "no active turn to interrupt"),
        ];
        for (code, message) in cases {
            let mut h = harness().await;
            let client = h.client.with_request_timeout(Duration::from_secs(5));
            let interrupt = client.turn_interrupt("thread-1", "turn-1");

            let server_task = tokio::spawn(async move {
                let req = server_recv_json(&mut h.server).await;
                let id = req.get("id").cloned().unwrap();
                server_send_json(
                    &mut h.server,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": code, "message": message },
                    }),
                )
                .await;
                h.server
            });

            let error = interrupt
                .await
                .expect_err("only Codex's exact no-active-turn response may be ignored");
            let CalmError::CodexRefused(actual) = error else {
                panic!("RPC refusals must retain their error type: {error}");
            };
            assert_eq!(
                actual,
                format!("turn/interrupt failed: {message} (code {code})")
            );
            let _server = server_task.await.unwrap();
        }
    }

    /// A never-answered request times out and leaves NO entry in the pending map.
    #[tokio::test]
    async fn never_answered_request_times_out_and_cleans_pending() {
        let h = harness().await;
        // Keep the server end alive but silent — never reply.
        let _server = h.server;
        let client = h.client.with_request_timeout(Duration::from_millis(50));

        let err = client
            .request::<Value>("thread/start", json!({}))
            .await
            .expect_err("a never-answered request must error");
        match err {
            CalmError::CodexAppServer(msg) => {
                assert!(
                    msg.contains("thread/start") && msg.contains("timed out"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected CodexAppServer timeout error, got {other:?}"),
        }

        // The pending entry for the timed-out request must be gone.
        assert!(
            client.pending.lock().unwrap().is_empty(),
            "pending map must not leak the timed-out request"
        );
    }

    /// A caller-supplied deadline must clean the pending map the same way the per-client one does: an outer `tokio::time::timeout` would drop the future before its own elapse arm runs and leak the entry.
    #[tokio::test]
    async fn a_caller_deadline_cleans_pending_the_same_way() {
        let h = harness().await;
        // Server end stays alive and silent.
        let _server = h.server;
        let client = h.client;

        let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
        let err = client
            .request_until::<Value>("model/list", json!({}), deadline)
            .await
            .expect_err("a never-answered request must error");
        match err {
            CalmError::CodexAppServer(msg) => {
                assert!(
                    msg.contains("model/list") && msg.contains("timed out"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected CodexAppServer timeout error, got {other:?}"),
        }

        assert!(
            client.pending.lock().unwrap().is_empty(),
            "a caller-supplied deadline must not leak the timed-out request"
        );
    }

    /// A budget that is already spent answers without putting a frame on the
    /// wire — and therefore also without registering a pending entry.
    #[tokio::test]
    async fn an_expired_deadline_never_reaches_the_wire() {
        let h = harness().await;
        let _server = h.server;
        let client = h.client;

        let spent = tokio::time::Instant::now() - Duration::from_secs(1);
        let err = client
            .request_until::<Value>("config/read", json!({}), spent)
            .await
            .expect_err("a spent budget must not be waited on");
        match err {
            CalmError::CodexAppServer(msg) => assert!(
                msg.contains("config/read") && msg.contains("budget"),
                "unexpected error message: {msg}"
            ),
            other => panic!("expected CodexAppServer error, got {other:?}"),
        }
        assert!(
            client.pending.lock().unwrap().is_empty(),
            "nothing was sent, so nothing may be pending"
        );
    }

    /// With many notifications queued and NO consumer draining them, a real RPC response still routes back to the waiting request.
    #[tokio::test]
    async fn response_routes_while_notifications_are_undrained() {
        let mut h = harness().await;
        // Do NOT drain `h.notifs` — let notifications pile up unbounded.

        // Flood the reader with notifications first.
        for i in 0..1000u64 {
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "method": "item/agentMessage/delta",
                    "params": { "delta": format!("n{i}") },
                }),
            )
            .await;
        }

        // If notification delivery could block the reader loop, this response would never be routed and the request would time out.
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.request::<ThreadResult>("thread/start", json!({}));

        // Server side: read the request frame, echo a response for its id.
        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "abc" }, "model": "gpt-5.5" },
                }),
            )
            .await;
            // Keep the connection open so the reader doesn't tear down.
            h.server
        });

        let result = req_fut
            .await
            .expect("response must route despite the notification backlog");
        assert_eq!(result.thread_id(), Some("abc"));
        let _server = server_task.await.unwrap();
    }

    /// Response correlation also works when the server echoes the id as a string.
    #[tokio::test]
    async fn response_correlates_with_string_id() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.request::<ThreadResult>("thread/start", json!({}));

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            let id = req.get("id").and_then(Value::as_u64).unwrap();
            // Echo the id back as a STRING — the reader must still correlate.
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id.to_string(),
                    "result": { "thread": { "id": "str-id" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("string-id response must correlate");
        assert_eq!(result.thread_id(), Some("str-id"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_start_with_params_sends_runtime_fields() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.thread_start_with_params(ThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: None,
        });

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/start")
            );
            let params = req.get("params").expect("params");
            assert_eq!(
                params.get("cwd").and_then(Value::as_str),
                Some("/workspace")
            );
            assert_eq!(
                params.get("approvalPolicy").and_then(Value::as_str),
                Some("never")
            );
            assert_eq!(
                params.get("sandbox").and_then(Value::as_str),
                Some("workspace-write")
            );
            assert!(
                !params
                    .as_object()
                    .is_some_and(|params| params.contains_key("additionalContext")),
                "PR5 must not send additionalContext: {req}"
            );
            assert!(
                !params
                    .as_object()
                    .is_some_and(|params| params.contains_key("config")),
                "thread/start must omit config when None: {req}"
            );

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "with-runtime-fields" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("thread/start");
        assert_eq!(result.thread_id(), Some("with-runtime-fields"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_start_with_params_sends_config_when_some() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let expected = json!({
            "shell_environment_policy": {
                "set": {
                    "NEIGE_MCP_SOCKET": "/tmp/calm.sock",
                    "NEIGE_MCP_TOKEN": "raw-per-card",
                }
            }
        });
        let req_fut = client.thread_start_with_params(ThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: Some(expected.clone()),
        });

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/start")
            );
            let params = req.get("params").expect("params");
            assert_eq!(params.get("config"), Some(&expected));

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "with-config" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("thread/start");
        assert_eq!(result.thread_id(), Some("with-config"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_start_with_params_omits_config_when_none() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.thread_start_with_params(ThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: None,
        });

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/start")
            );
            let params = req.get("params").expect("params");
            assert!(!params.as_object().unwrap().contains_key("config"));

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "without-config" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("thread/start");
        assert_eq!(result.thread_id(), Some("without-config"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_resume_with_config_sends_config_when_some() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let expected = json!({
            "shell_environment_policy": {
                "set": {
                    "NEIGE_MCP_SOCKET": "/tmp/calm.sock",
                    "NEIGE_MCP_TOKEN": "raw-per-card",
                }
            }
        });
        let req_fut = client.thread_resume_with_config("thread-123", Some(expected.clone()));

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            let params = req.get("params").expect("params");
            assert_eq!(
                params.get("threadId").and_then(Value::as_str),
                Some("thread-123")
            );
            assert_eq!(params.get("config"), Some(&expected));

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "thread-123" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("thread/resume");
        assert_eq!(result.thread_id(), Some("thread-123"));
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn thread_resume_with_config_omits_config_when_none() {
        let mut h = harness().await;
        let client = h.client.with_request_timeout(Duration::from_secs(5));
        let req_fut = client.thread_resume_with_config("thread-456", None);

        let server_task = tokio::spawn(async move {
            let req = server_recv_json(&mut h.server).await;
            assert_eq!(
                req.get("method").and_then(Value::as_str),
                Some("thread/resume")
            );
            let params = req.get("params").expect("params");
            assert_eq!(
                params.get("threadId").and_then(Value::as_str),
                Some("thread-456")
            );
            assert!(!params.as_object().unwrap().contains_key("config"));

            let id = req.get("id").cloned().unwrap();
            server_send_json(
                &mut h.server,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "thread": { "id": "thread-456" }, "model": "m" },
                }),
            )
            .await;
            h.server
        });

        let result = req_fut.await.expect("thread/resume");
        assert_eq!(result.thread_id(), Some("thread-456"));
        let _server = server_task.await.unwrap();
    }

    #[test]
    fn input_item_text_serializes_to_schema_shape() {
        let item = InputItem::text("hello");
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v, json!({ "type": "text", "text": "hello" }));
    }

    #[test]
    fn thread_result_plucks_id_and_tolerates_extra_fields() {
        // Extra top-level + nested fields must not break deserialization.
        let raw = json!({
            "thread": { "id": "abc-123", "status": { "type": "idle" }, "turns": [] },
            "model": "gpt-5.5",
            "cwd": "/tmp",
            "approvalPolicy": "never",
            "unknownFutureField": 42
        });
        let r: ThreadResult = serde_json::from_value(raw).unwrap();
        assert_eq!(r.thread_id(), Some("abc-123"));
        assert_eq!(r.model, "gpt-5.5");
    }

    /// The assertion is on the frame, because the frame is the entire contract.
    #[test]
    fn a_chosen_model_and_effort_reach_the_turn_start_frame() {
        let frame = turn_start_params(
            "thread-1",
            &[InputItem::text("hi")],
            &TurnModelSelection {
                model: Some("gpt-5".into()),
                effort: Some("high".into()),
            },
            None,
        );
        assert_eq!(frame["threadId"], json!("thread-1"));
        assert_eq!(frame["model"], json!("gpt-5"));
        assert_eq!(frame["effort"], json!("high"));
    }

    /// Asserted in both directions so a fixture whose two identifiers are equal cannot let a read of the wrong field pass.
    #[test]
    fn the_frame_carries_a_slug_and_never_a_preset_id() {
        let catalog_entry = json!({ "id": "preset-abc", "model": "gpt-5" });
        let frame = turn_start_params(
            "thread-1",
            &[],
            &TurnModelSelection {
                model: catalog_entry["model"].as_str().map(ToOwned::to_owned),
                effort: None,
            },
            None,
        );
        assert_eq!(frame["model"], json!("gpt-5"));
        assert_ne!(frame["model"], json!("preset-abc"));
    }

    /// `inherit` sends no `model`, no `effort`, and in particular no explicit `null`.
    #[test]
    fn inherit_sends_neither_key_and_not_a_null_either() {
        let frame = turn_start_params("thread-1", &[], &TurnModelSelection::inherit(), None);
        let map = frame.as_object().expect("params is an object");
        assert!(!map.contains_key("model"), "frame was {frame}");
        assert!(!map.contains_key("effort"), "frame was {frame}");
        assert_eq!(map.len(), 2, "only threadId and input: {frame}");
    }

    /// Half a selection puts half a frame on the wire; the absent half stays
    /// absent rather than becoming a null.
    #[test]
    fn each_key_is_omitted_independently() {
        let model_only = turn_start_params(
            "t",
            &[],
            &TurnModelSelection {
                model: Some("gpt-5".into()),
                effort: None,
            },
            None,
        );
        assert_eq!(model_only["model"], json!("gpt-5"));
        assert!(!model_only.as_object().unwrap().contains_key("effort"));

        let effort_only = turn_start_params(
            "t",
            &[],
            &TurnModelSelection {
                model: None,
                effort: Some("low".into()),
            },
            None,
        );
        assert_eq!(effort_only["effort"], json!("low"));
        assert!(!effort_only.as_object().unwrap().contains_key("model"));
    }

    /// Codex accepts any non-empty effort string, so an unrecognised one must reach the wire unaltered.
    #[test]
    fn an_unrecognised_effort_string_is_not_filtered_out() {
        let frame = turn_start_params(
            "t",
            &[],
            &TurnModelSelection {
                model: None,
                effort: Some("ludicrous".into()),
            },
            None,
        );
        assert_eq!(frame["effort"], json!("ludicrous"));
    }

    /// The drain's client id reaches the frame under codex's own key, and its absence is an absent key rather than `null`.
    #[test]
    fn a_client_user_message_id_reaches_the_frame_and_is_omitted_otherwise() {
        let frame = turn_start_params("t", &[], &TurnModelSelection::inherit(), Some("entry-0001"));
        assert_eq!(frame["clientUserMessageId"], json!("entry-0001"));
        let bare = turn_start_params("t", &[], &TurnModelSelection::inherit(), None);
        assert!(
            !bare
                .as_object()
                .unwrap()
                .contains_key("clientUserMessageId"),
            "frame was {bare}"
        );
    }

    /// The steer frame carries the three required keys plus the client id, omitted rather than `null` when absent; no `model`, no `effort`.
    #[test]
    fn a_steer_frame_carries_the_client_id_and_omits_it_otherwise() {
        let input = vec![InputItem::text("now")];
        let frame = turn_steer_params("t", "turn-7", &input, Some("entry-0002"));
        assert_eq!(frame["threadId"], json!("t"));
        assert_eq!(frame["expectedTurnId"], json!("turn-7"));
        assert_eq!(frame["input"], serde_json::to_value(&input).unwrap());
        assert_eq!(frame["clientUserMessageId"], json!("entry-0002"));
        let keys = frame
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 4, "no settings ride on a steer: {keys:?}");

        let bare = turn_steer_params("t", "turn-7", &input, None);
        assert!(
            !bare
                .as_object()
                .unwrap()
                .contains_key("clientUserMessageId"),
            "frame was {bare}"
        );
        assert_eq!(bare.as_object().unwrap().len(), 3);
    }

    #[test]
    fn turn_start_result_plucks_turn_id() {
        let raw = json!({ "turn": { "id": "turn-9", "status": "inProgress", "items": [] } });
        let r: TurnStartResult = serde_json::from_value(raw).unwrap();
        assert_eq!(r.turn_id(), Some("turn-9"));
    }

    #[test]
    fn turn_steer_result_camel_case() {
        let raw = json!({ "turnId": "turn-42" });
        let r: TurnSteerResult = serde_json::from_value(raw).unwrap();
        assert_eq!(r.turn_id, "turn-42");
    }

    #[test]
    fn initialize_result_tolerates_unknown_fields() {
        let raw = json!({
            "userAgent": "codex/0.133.0",
            "codexHome": "/home/x/.codex",
            "platformFamily": "unix",
            "platformOs": "linux",
            "somethingNew": true
        });
        let r: InitializeResult = serde_json::from_value(raw).unwrap();
        assert_eq!(r.platform_os, "linux");
    }

    #[test]
    fn notification_parse_maps_known_methods() {
        let n = Notification::parse(
            "turn/completed".into(),
            json!({ "threadId": "t1", "turn": { "id": "u1" } }),
        );
        match n {
            Notification::TurnCompleted { thread_id, turn } => {
                assert_eq!(thread_id, "t1");
                assert_eq!(turn.get("id").and_then(Value::as_str), Some("u1"));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }

        let n = Notification::parse(
            "thread/status/changed".into(),
            json!({ "threadId": "t1", "status": { "type": "active", "activeFlags": [] } }),
        );
        assert!(matches!(n, Notification::ThreadStatusChanged { .. }));

        let n = Notification::parse("item/agentMessage/delta".into(), json!({ "delta": "x" }));
        match n {
            Notification::Item { method, .. } => assert_eq!(method, "item/agentMessage/delta"),
            other => panic!("expected Item, got {other:?}"),
        }
    }

    /// `turn/plan/updated` needs no variant: it must reach the run loop through `Other` with `params` unchanged (`Value`-level equality) and `thread_id()` resolving from the top-level `threadId`.
    #[test]
    fn notification_parse_preserves_turn_plan_updated_frame() {
        let params = json!({
            "threadId": "t-plan",
            "turnId": "turn-plan-1",
            "explanation": null,
            "plan": [
                { "step": "first", "status": "inProgress" },
                { "step": "second", "status": "pending" }
            ]
        });
        let n = Notification::parse("turn/plan/updated".into(), params.clone());
        assert_eq!(n.thread_id(), Some("t-plan"));
        match n {
            Notification::Other {
                method,
                params: got,
            } => {
                assert_eq!(method, "turn/plan/updated");
                assert_eq!(
                    got, params,
                    "params must survive parse with no field dropped, added or reshaped"
                );
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn notification_parse_unknown_method_is_other_not_error() {
        let n = Notification::parse("thread/realtime/sdp".into(), json!({ "anything": 1 }));
        match n {
            Notification::Other { method, .. } => assert_eq!(method, "thread/realtime/sdp"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn notification_thread_id_reads_direct_variants() {
        let status = Notification::ThreadStatusChanged {
            thread_id: "thread-status".into(),
            status: json!({ "type": "idle" }),
        };
        let started = Notification::TurnStarted {
            thread_id: "thread-started".into(),
            turn: json!({ "id": "turn-1" }),
        };
        let completed = Notification::TurnCompleted {
            thread_id: "thread-completed".into(),
            turn: json!({ "id": "turn-1" }),
        };

        assert_eq!(status.thread_id(), Some("thread-status"));
        assert_eq!(started.thread_id(), Some("thread-started"));
        assert_eq!(completed.thread_id(), Some("thread-completed"));
    }

    #[test]
    fn notification_thread_id_reads_thread_started_params() {
        let nested = Notification::ThreadStarted {
            params: json!({ "thread": { "id": "thread-nested" } }),
        };
        let flat = Notification::ThreadStarted {
            params: json!({ "threadId": "thread-flat" }),
        };

        assert_eq!(nested.thread_id(), Some("thread-nested"));
        assert_eq!(flat.thread_id(), Some("thread-flat"));
    }

    #[test]
    fn notification_thread_id_reads_item_and_other_params() {
        let item = Notification::Item {
            method: "item/completed".into(),
            params: json!({ "threadId": "thread-item" }),
        };
        let other = Notification::Other {
            method: "approval/request".into(),
            params: json!({ "threadId": "thread-other" }),
        };

        assert_eq!(item.thread_id(), Some("thread-item"));
        assert_eq!(other.thread_id(), Some("thread-other"));
    }

    /// Asserts the exact bytes because the only reader is codex, whose types are not a compilable dependency: deleting the variant-level `#[serde(rename = "localImage")]` turns only this red.
    #[test]
    fn local_image_serializes_with_the_camel_case_tag() {
        let json = serde_json::to_value(InputItem::local_image("/w/.neige/a.png")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "localImage", "path": "/w/.neige/a.png"}),
        );
        // The sibling variant is unaffected: the rename is scoped to the one variant that needs it.
        assert_eq!(
            serde_json::to_value(InputItem::text("hi")).unwrap(),
            serde_json::json!({"type": "text", "text": "hi"}),
        );
    }

    /// `detail` is optional on codex's side and we deliberately send nothing.
    #[test]
    fn a_local_image_item_has_exactly_two_keys() {
        let json = serde_json::to_value(InputItem::local_image("/w/a.png")).unwrap();
        let object = json.as_object().expect("an input item is an object");
        let mut keys = object.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, vec!["path".to_string(), "type".to_string()]);
    }
}
