//! Programmatic client for a card's `codex app-server` (issue #293 PR2).
//!
//! The push migration (#293) attaches a daemon-side control/observe
//! channel to the *same* codex thread a human's `--remote unix://` TUI is
//! driving, so the spec agents can switch from polling the kernel to a
//! push subscription on the codex event stream. PR1's spike
//! (`docs/spikes/293-appserver-thread-sharing.md`) verified this is
//! possible against the real binary (codex-cli 0.133.0); this module is
//! the Rust client that implements it. **It is NOT wired into the
//! dispatcher yet** — that is PR3.
//!
//! ## Wire protocol (from the spike — build to these exactly)
//!
//! `codex app-server --listen unix://PATH` speaks **WebSocket over the
//! Unix domain socket**, carrying JSON-RPC 2.0 messages as WebSocket text
//! frames (URI path `ws://localhost/`). Two hard facts:
//!
//!   * **`permessage-deflate` MUST NOT be offered** or the server rejects
//!     the handshake (`Missing, duplicated or incorrect header
//!     sec-websocket-extensions`). We use `tokio-tungstenite` 0.24, whose
//!     handshake never offers compression (no `Sec-WebSocket-Extensions`
//!     header is generated — confirmed against the crate source), so this
//!     is satisfied *by construction*. The hand-built request below adds
//!     no extension header either. This is the Rust equivalent of the
//!     spike's Python `compression=None`.
//!   * Raw JSON written to the socket without a WS upgrade is silently
//!     dropped (connection closed, zero bytes). We always go through the
//!     WS client.
//!
//! JSON-RPC envelope: `{"jsonrpc":"2.0","id":<int|string>,"method":"…","params":…}`.
//! The `jsonrpc` field is optional on the wire but we send it. We always
//! emit integer ids; the protocol permits string ids too, so the reader
//! correlates responses by an id that is either an integer or a
//! string-encoded integer (defensive — we control the ids). Response
//! `id` echoes the request; notifications carry no `id`. The connection is
//! tagged `api_version=v2` server-side; we send `capabilities.experimentalApi
//! = true` in `initialize` because all the methods we use are `[experimental]`.
//!
//! ## Architecture
//!
//! [`CodexAppServer::connect`] opens the WS-over-UDS connection, spawns a
//! background **reader task** that owns the WS read half, and returns a
//! handle plus a [`NotificationStream`]. The reader demultiplexes incoming
//! frames:
//!
//!   * **responses** (frames with an `id` we are waiting on) are routed to
//!     the matching request via a per-id [`oneshot`] channel held in a
//!     shared pending-map, and
//!   * **notifications** (frames with a `method` and no `id`, plus error
//!     frames whose id we are not tracking) are parsed into [`Notification`]
//!     and pushed onto an mpsc channel the caller consumes.
//!
//! Request methods serialize params, register a oneshot, write the frame
//! through a `Mutex`-guarded write half, and await the correlated response.
//! Unknown notification methods become [`Notification::Other`] rather than
//! an error so codex version drift never breaks the consumer.
//!
//! ## Schema scope
//!
//! We type ONLY the params/results we use (not all 257 schema types).
//! Every result struct is `#[serde(default)]` / ignores unknown fields so
//! the `[experimental]` protocol can grow fields without breaking us. The
//! thread/turn objects are kept as `serde_json::Value` where we only need
//! to pluck an id — vendoring their full shape buys nothing for PR2.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
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

/// WebSocket URI the server expects over the UDS. The host is irrelevant
/// (there is no DNS over a unix socket) but tungstenite requires a `Host`
/// header — `localhost` matches what the spike used.
const WS_URI: &str = "ws://localhost/";

/// Default per-request response timeout. All RPC methods we call return a
/// short *acknowledgement* (e.g. `turn/start` returns only the turn id; turn
/// *completion* arrives later as a `turn/completed` notification), so a tight
/// bound never truncates a long-running turn. We pick 30 s — generous for a
/// model-adjacent ack while still bounding a hung/never-answered request.
/// (`plugin_host/mcp.rs:373` uses 10 s for its purely-local handshake.)
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// Notification backpressure decision (issue #293 fix-loop):
//
// The notification channel is **unbounded** (`mpsc::unbounded_channel`).
// Responses and notifications are demultiplexed on the *same* reader loop;
// responses are delivered via non-blocking `oneshot::send`. If notification
// delivery could ever block the reader (as a bounded channel's
// `send().await` does when full), a slow/absent notification consumer would
// stall ALL in-flight RPC responses — a latent deadlock. Using an unbounded
// channel with the synchronous, never-awaiting `unbounded_send` keeps
// notification delivery from ever blocking response routing, and guarantees
// turn-lifecycle notifications (esp. `turn/completed`, which PR3's dispatcher
// depends on) are never silently dropped. The trade-off is unbounded memory
// if a consumer never drains while events keep arriving; that is acceptable
// here because the consumer (PR3) drains promptly and the connection is
// per-card and short-lived. The alternative — bounded `try_send` with a drop
// counter — was rejected because dropping `turn/completed` is unacceptable.

// ===========================================================================
// Typed params / results — only what we use.
// ===========================================================================

/// `clientInfo` block for `initialize`. Required by the schema.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// `capabilities` block for `initialize`. All methods we call are
/// `[experimental]`, so we always set `experimentalApi=true`.
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

/// `initialize` result. Tolerates extra fields (the schema lists exactly
/// these four as required, but we only assert on shape, not values).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InitializeResult {
    pub user_agent: String,
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
}

/// A single `turn/start` / `turn/steer` input item. Only the `text`
/// variant is modeled (the spike + our deterministic prompts use it); the
/// schema also has `image`/file variants we don't need yet.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InputItem {
    /// `{"type":"text","text":"…"}`.
    Text { text: String },
}

#[derive(Clone)]
pub struct ThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub developer_instructions: Option<String>,
    pub config: Option<serde_json::Value>,
}

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
    redacted
}

impl InputItem {
    /// Convenience constructor for the text variant.
    pub fn text(s: impl Into<String>) -> Self {
        InputItem::Text { text: s.into() }
    }
}

/// `thread/start` / `thread/resume` result. The full thread object is kept
/// as a `Value` — we only ever read `thread.id`, exposed via
/// [`ThreadResult::thread_id`].
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ThreadResult {
    /// Raw `thread` object from the server.
    pub thread: Value,
    /// Resolved model (e.g. `gpt-5.5`).
    pub model: String,
}

impl ThreadResult {
    /// The thread id (`thread.id`), the handle every subsequent
    /// `turn/*` / `thread/*` call keys on. `None` only if the server
    /// returned a shape without it (should not happen on success).
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
    /// The id of the started turn — needed as `expectedTurnId` for a
    /// subsequent `turn/steer` and as `turnId` for `turn/interrupt`.
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

// ===========================================================================
// Notification stream (server -> client).
// ===========================================================================

/// A server→client notification, narrowed to the variants the push
/// migration cares about. Anything else (the dozens of housekeeping /
/// realtime / approval methods) lands in [`Notification::Other`] so codex
/// version drift never breaks the consumer.
///
/// The `method` strings map per the v2 schema (`ServerNotification`).
#[derive(Debug, Clone)]
pub enum Notification {
    /// `thread/started` — a thread was created/loaded on this connection.
    ThreadStarted { params: Value },
    /// `thread/status/changed` — `status` is `{ "type": "idle" | "active"
    /// | "notLoaded" | "systemError", … }`. The `active` variant carries
    /// `activeFlags`. We keep the raw status `Value` plus the thread id.
    ThreadStatusChanged { thread_id: String, status: Value },
    /// `turn/started` — carries `threadId` + the full `turn` object.
    TurnStarted { thread_id: String, turn: Value },
    /// `turn/completed` — the terminal event of a turn; carries `threadId`
    /// + the final `turn` object.
    TurnCompleted { thread_id: String, turn: Value },
    /// Any `item/*` event (`item/started`, `item/completed`,
    /// `item/agentMessage/delta`, …). The exact method is preserved so a
    /// consumer can branch without re-deriving it.
    Item { method: String, params: Value },
    /// Any method we don't model. Preserves `method` + `params` so nothing
    /// is silently lost.
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

    /// Parse a notification frame's `method` + `params` into a typed
    /// variant. Never fails: unknown / malformed shapes degrade to
    /// [`Notification::Other`] (or keep raw params), keeping the consumer
    /// resilient to protocol drift.
    fn parse(method: String, params: Value) -> Self {
        // Best-effort field extraction; missing fields default to empty.
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

/// The receiving half of the server→client notification stream. Owned by
/// the caller; the reader task pushes [`Notification`]s onto it. When the
/// connection closes the channel ends (`recv` returns `None`).
pub struct NotificationStream {
    rx: mpsc::UnboundedReceiver<Notification>,
}

impl NotificationStream {
    /// Await the next notification, or `None` once the connection closed.
    pub async fn recv(&mut self) -> Option<Notification> {
        self.rx.recv().await
    }

    /// Await the next notification, returning a deterministic error once
    /// the stream ends because the server closed the connection, the WS
    /// reader hit an error, or the reader task exited.
    pub async fn recv_result(&mut self) -> Result<Notification> {
        self.rx.recv().await.ok_or_else(notification_stream_closed)
    }

    /// Await the next notification satisfying `predicate`.
    ///
    /// Non-matching notifications are consumed. If the underlying stream
    /// ends before a match arrives, returns a deterministic error instead
    /// of silently yielding `None`, so callers can race this future against
    /// process exit in a `tokio::select!`.
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

// ===========================================================================
// JSON-RPC framing helpers.
// ===========================================================================

/// In-flight request registry: JSON-RPC id -> sender for its response.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;

/// A JSON-RPC error object as returned by the server (`-32600` etc.).
#[derive(Debug, Clone, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// The write half of the socket, behind a `Mutex` so concurrent request
/// methods serialize their frame writes (WS frames must not interleave).
type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocketStream<UnixStream>, Message>>>;

// ===========================================================================
// The client.
// ===========================================================================

/// An async client for one card's `codex app-server`, connected over
/// WebSocket-over-UDS. Cheaply cloneable is intentionally NOT provided:
/// the write sink is `Arc`-shared internally so `&self` methods can be
/// called concurrently, but you hold a single handle.
pub struct CodexAppServer {
    sink: WsSink,
    pending: Pending,
    next_id: AtomicU64,
    /// Per-request response timeout. This is a leak/wedge backstop for a
    /// request whose response never arrives; lifecycle state is driven by
    /// notifications / EOF / child exit, not by this timer. Configurable via
    /// [`CodexAppServer::set_request_timeout`]; defaults to
    /// [`DEFAULT_REQUEST_TIMEOUT`].
    request_timeout: Duration,
    /// Kept so dropping the client aborts the reader task (no orphan task
    /// after the connection is gone).
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl CodexAppServer {
    /// Test-only: build a fully-constructed [`CodexAppServer`] over an
    /// in-process `UnixStream::pair` WebSocket handshake, returning the
    /// client + its [`NotificationStream`] + the *server* end (which the
    /// caller must keep alive — dropping it closes the connection and
    /// stops the reader). Lets other modules' tests (e.g.
    /// `spec_appserver`) construct a real client/handle without a `codex`
    /// binary. The server end is returned rather than parked so the
    /// caller controls its lifetime.
    #[cfg(test)]
    pub(crate) async fn connect_pair_for_test()
    -> (Self, NotificationStream, WebSocketStream<UnixStream>) {
        let (client_io, server_io) = UnixStream::pair().expect("unix socket pair");
        let req = WS_URI.into_client_request().unwrap();
        let client_fut = tokio_tungstenite::client_async(req, client_io);
        let server_fut = tokio_tungstenite::accept_async(server_io);
        let (client_res, server_res) = tokio::join!(client_fut, server_fut);
        let (client_ws, _resp) = client_res.expect("client handshake");
        let server = server_res.expect("server handshake");

        let (write, read) = client_ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(reader_loop(read, pending.clone(), notif_tx));
        let client = Self {
            sink,
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reader,
        };
        (client, NotificationStream { rx: notif_rx }, server)
    }

    /// Connect to a `codex app-server` listening on the unix socket at
    /// `sock_path` (started with `--listen unix://<sock_path>`). Performs
    /// the WebSocket upgrade (compression disabled by construction), spawns
    /// the background reader, and returns the client paired with its
    /// [`NotificationStream`]. Does NOT send `initialize` — call
    /// [`CodexAppServer::initialize`] next.
    pub async fn connect(sock_path: impl AsRef<Path>) -> Result<(Self, NotificationStream)> {
        let sock_path = sock_path.as_ref();
        let stream = UnixStream::connect(sock_path).await.map_err(|e| {
            CalmError::CodexAppServer(format!("connect unix socket {}: {e}", sock_path.display()))
        })?;

        // Build the handshake request by hand. `IntoClientRequest` on a
        // `&str` URI fills in the mandatory Sec-WebSocket-* headers and a
        // `Host`; crucially it adds NO `Sec-WebSocket-Extensions`, so we
        // never offer permessage-deflate (the server would otherwise
        // reject the upgrade — see module docs).
        let request = WS_URI
            .into_client_request()
            .map_err(|e| CalmError::CodexAppServer(format!("build ws handshake request: {e}")))?;

        let (ws, _resp) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| {
                CalmError::CodexAppServer(format!("ws handshake over {}: {e}", sock_path.display()))
            })?;

        let (write, read) = ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        // Unbounded: notification delivery must never block the reader's
        // response routing — see the backpressure note at the top of this
        // module.
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        let reader = tokio::spawn(reader_loop(read, pending.clone(), notif_tx));

        tracing::debug!(sock = %sock_path.display(), "codex app-server: connected");

        let client = Self {
            sink,
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reader,
        };
        Ok((client, NotificationStream { rx: notif_rx }))
    }

    /// Override the per-request response timeout (default
    /// [`DEFAULT_REQUEST_TIMEOUT`] = 30 s). Builder-style so a caller can
    /// `CodexAppServer::connect(..).await?.0.with_request_timeout(..)`.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// The current per-request response timeout.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Send `initialize` and return its result. Must be the first call on
    /// a fresh connection (the server requires it before any other method).
    pub async fn initialize(&self, client_info: ClientInfo) -> Result<InitializeResult> {
        let params = InitializeParams {
            client_info,
            capabilities: InitializeCapabilities {
                experimental_api: true,
            },
        };
        self.request("initialize", json!(params)).await
    }

    /// `thread/start` — create a fresh thread. Note (from the spike): a
    /// brand-new thread has NO rollout on disk until a turn runs, so a
    /// second connection cannot `thread/resume` it until then.
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
        let mut value = json!({
            "cwd": params.cwd,
            "approvalPolicy": params.approval_policy,
            "sandbox": params.sandbox_mode,
        });
        if let Some(prompt) = params.developer_instructions {
            value["developerInstructions"] = Value::String(prompt);
        }
        if let Some(config) = params.config {
            value["config"] = config;
        }
        self.request("thread/start", value).await
    }

    /// `thread/resume` — attach to an existing thread by id. Fails with a
    /// `-32600 "no rollout found …"` (surfaced as
    /// [`CalmError::CodexAppServer`]) if the thread has not yet run a turn.
    pub async fn thread_resume(&self, thread_id: &str) -> Result<ThreadResult> {
        self.request("thread/resume", json!({ "threadId": thread_id }))
            .await
    }

    /// `turn/start` — begin a turn on `thread_id` with the given input.
    /// Returns quickly with the started turn's id; the actual work streams
    /// as notifications (`turn/started` → `item/*` → `turn/completed`).
    pub async fn turn_start(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
    ) -> Result<TurnStartResult> {
        self.request(
            "turn/start",
            json!({ "threadId": thread_id, "input": input }),
        )
        .await
    }

    /// `turn/steer` — redirect an in-flight turn. `expected_turn_id` must
    /// be the id returned by the `turn/start` that is still running.
    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
    ) -> Result<TurnSteerResult> {
        self.request(
            "turn/steer",
            json!({
                "threadId": thread_id,
                "expectedTurnId": expected_turn_id,
                "input": input,
            }),
        )
        .await
    }

    /// `thread/inject_items` — push context items onto a thread without
    /// starting a turn. `items` is an opaque array of schema item objects
    /// (kept as `Value` — the shape is large and we don't constrain it).
    /// Returns `()` (the server returns `{}`). Note: inject alone does not
    /// create a rollout, so it does not make a turn-less thread resumable.
    pub async fn inject_items(&self, thread_id: &str, items: Vec<Value>) -> Result<()> {
        let _: Value = self
            .request(
                "thread/inject_items",
                json!({ "threadId": thread_id, "items": items }),
            )
            .await?;
        Ok(())
    }

    /// `turn/interrupt` — cancel a running turn. Both `thread_id` and the
    /// running `turn_id` are required.
    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        let _: Value = self
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await?;
        Ok(())
    }

    /// Core request/response round-trip: assign an id, register a oneshot,
    /// write the frame, await the correlated response, deserialize the
    /// `result` into `T`. A JSON-RPC `error` frame, a transport failure, or
    /// the reader task dying all map to [`CalmError::CodexAppServer`].
    async fn request<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let text = serde_json::to_string(&frame)?;

        // Write under the sink lock so concurrent requests don't interleave
        // WS frames.
        {
            let mut sink = self.sink.lock().await;
            if let Err(e) = sink.send(Message::Text(text)).await {
                // Drop the now-unanswerable pending entry.
                self.pending.lock().await.remove(&id);
                return Err(CalmError::CodexAppServer(format!("send {method}: {e}")));
            }
        }

        tracing::trace!(id, method, "codex app-server: request sent");

        // Backstop the await: a server that never answers (or an event
        // we'll never get a response for) must not hang the caller forever.
        // On elapse we remove our own pending entry so the map doesn't leak
        // the never-fired oneshot. NOTE: `turn/start` returns only the turn
        // *ack* (turn id); turn lifecycle is decided later by
        // notifications/EOF/child-exit, so this timer is not a turn
        // lifecycle criterion.
        let outcome = match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(received) => received,
            Err(_elapsed) => {
                self.pending.lock().await.remove(&id);
                let secs = self.request_timeout.as_secs_f64();
                return Err(CalmError::CodexAppServer(format!(
                    "request {method} timed out after {secs}s"
                )));
            }
        };

        match outcome {
            Ok(Ok(value)) => serde_json::from_value(value)
                .map_err(|e| CalmError::CodexAppServer(format!("decode {method} result: {e}"))),
            Ok(Err(rpc)) => Err(CalmError::CodexAppServer(format!(
                "{method} failed: {} (code {})",
                rpc.message, rpc.code
            ))),
            // Sender dropped without sending: the reader task ended (the
            // connection closed) before our response arrived.
            Err(_) => Err(CalmError::CodexAppServer(format!(
                "{method}: connection closed before response"
            ))),
        }
    }
}

/// Background reader: owns the WS read half, demultiplexes every inbound
/// frame into either a response (routed to the matching pending request)
/// or a notification (pushed onto the caller's channel). Exits when the WS
/// closes, on a transport error, or when the notification consumer is gone.
async fn reader_loop(
    mut read: futures_util::stream::SplitStream<WebSocketStream<UnixStream>>,
    pending: Pending,
    notif_tx: mpsc::UnboundedSender<Notification>,
) {
    while let Some(frame) = read.next().await {
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
            // Ping/Pong/Close/frame — tungstenite auto-replies to pings;
            // a Close ends the stream on the next poll.
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

        // A frame with an `id` we are tracking is a response. Everything
        // else (a `method` with no tracked id, or an untracked id) is a
        // notification we surface to the consumer.
        //
        // We emit integer ids, but the protocol permits string ids, so
        // match an integer first and fall back to a string-encoded integer
        // (defensive — keeps correlation robust if the server ever echoes
        // the id as a string).
        if let Some(id) = obj.get("id").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        }) {
            let sender = { pending.lock().await.remove(&id) };
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
                // Receiver may be gone if the request future was dropped;
                // that's fine.
                let _ = sender.send(payload);
                continue;
            }
            // Untracked id — fall through and treat as a notification if it
            // carries a method (rare), else drop.
        }

        if let Some(method) = obj.get("method").and_then(Value::as_str) {
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            let notif = Notification::parse(method.to_string(), params);
            // `unbounded_send` is synchronous and never awaits capacity, so a
            // slow/absent notification consumer can NEVER block RPC-response
            // routing on this same loop. The only failure is the consumer
            // having been dropped (receiver gone) — then the channel is
            // closed and we stop. See the backpressure note at the top of
            // this module.
            if notif_tx.send(notif).is_err() {
                tracing::debug!("codex app-server: notification consumer dropped; reader stopping");
                break;
            }
        }
    }

    // Connection ended: drain any pending requests so their futures resolve
    // with a clean "connection closed" error instead of hanging.
    let mut guard = pending.lock().await;
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test harness that wires a real WS-over-UnixStream connection: the
    /// client end is a fully-constructed [`CodexAppServer`] (real reader
    /// task), and the server end is a raw [`WebSocketStream`] the test drives
    /// directly to send responses / notifications (or to stay silent). This
    /// exercises `request()` and `reader_loop` end-to-end without a `codex`
    /// binary.
    struct Harness {
        client: CodexAppServer,
        /// The notification receiver. Most tests don't drain it (that's the
        /// point of the decoupling test), but it MUST stay alive: dropping
        /// it closes the channel, which would make the reader's
        /// `unbounded_send` fail and stop the reader. Underscored so the
        /// "never read" lint is satisfied while the value is kept.
        _notifs: NotificationStream,
        /// Server-side WS end; the test reads requests off it and writes
        /// responses/notifications back.
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

        let (write, read) = client_ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(reader_loop(read, pending.clone(), notif_tx));

        let client = CodexAppServer {
            sink,
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

    /// Fix #1: a request whose response never arrives times out, returns the
    /// `timed out` error, and leaves NO entry in the pending map (no leak).
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
            client.pending.lock().await.is_empty(),
            "pending map must not leak the timed-out request"
        );
    }

    /// Fix #2: notification delivery is decoupled from RPC response routing.
    /// With many notifications queued and NO consumer draining them, a real
    /// RPC response still routes back to the waiting request — proving the
    /// reader's unbounded notification send never blocks response delivery.
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

        // Now issue a real request; the server answers it. If notification
        // delivery could block the reader loop, this response would never be
        // routed and the request would hit its timeout instead.
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

    /// Cheap nit: response correlation also works when the server echoes the
    /// id as a *string* (`"1"`), not just an integer.
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
}
