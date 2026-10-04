//! Programmatic client for a card's `codex app-server` connection: JSON-RPC 2.0 over WebSocket over a unix socket.
//! `permessage-deflate` MUST NOT be offered or the server rejects the handshake; raw JSON without the WS upgrade is silently dropped. All methods used are `[experimental]`, so `initialize` sends `experimentalApi = true`.
//! A reader task demultiplexes responses (per-id oneshot), server requests, and notifications (unbounded mpsc); only the params/results we use are typed, and unknown fields are tolerated.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

pub mod error;
mod types;
pub use types::*;
mod client_transport;
#[cfg(test)]
mod server_request_tests;
mod server_requests;
pub mod tool_names;
use client_transport::{PendingRequest, TransportAbort};

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

use self::error::{Error, Result};
pub use crate::InputItem;
use crate::TurnModelSelection;

/// The host is irrelevant over a unix socket, but tungstenite requires a `Host` header.
const WS_URI: &str = "ws://localhost/";

/// Every RPC we call returns a short acknowledgement (turn completion arrives as a notification), so a tight bound never truncates a long turn.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound for `connect(2)` and the WebSocket upgrade: a peer whose accept loop is blocked lets `connect(2)` succeed (listen backlog) and then never sends the HTTP 101.
/// This handshake is purely local, so anything that has not answered in 10 s is wedged, not slow.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// The notification channel is unbounded on purpose: responses and notifications share one reader loop, so a blocking notification send would stall every in-flight RPC response.
// Dropping `turn/completed` is unacceptable, and the per-card consumer drains promptly.

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

fn notification_stream_closed() -> Error {
    Error::Transport(
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
    pending: Pending,
    next_id: AtomicU64,
    /// A leak/wedge backstop for a request whose response never arrives; lifecycle is driven by notifications / EOF / child exit, not by this timer.
    request_timeout: Duration,
    /// Kept so dropping the client aborts the reader task.
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
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
    /// Test-only: a fully-constructed [`CodexAppServer`] over an in-process `UnixStream::pair` handshake; the returned server end must be kept alive or the connection closes.
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

        let transport =
            Arc::new(TransportAbort::new(client_ws.get_ref()).expect("retain owned socket"));
        let (write, read) = client_ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(reader_loop(
            read,
            pending.clone(),
            notif_tx,
            sink.clone(),
            transport.clone(),
        ));
        let client = Self {
            sink,
            transport,
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
                    return Err(Error::Transport(format!(
                        "connect unix socket {}: {e}",
                        sock_path.display()
                    )));
                }
                Err(_) => {
                    return Err(Error::Transport(
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
            .map_err(|e| Error::Transport(format!("build ws handshake request: {e}")))?;

        let (ws, _resp) = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_tungstenite::client_async(request, stream),
        )
        .await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Err(Error::Transport(format!(
                    "ws handshake over {}: {e}",
                    sock_path.display()
                )));
            }
            Err(_) => {
                return Err(Error::Transport(
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

        let transport = Arc::new(
            TransportAbort::new(ws.get_ref())
                .map_err(|error| Error::Transport(format!("retain owned socket: {error}")))?,
        );
        let (write, read) = ws.split();
        let sink: WsSink = Arc::new(Mutex::new(write));
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        // Unbounded: notification delivery must never block the reader's response routing.
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        let reader = tokio::spawn(reader_loop(
            read,
            pending.clone(),
            notif_tx,
            sink.clone(),
            transport.clone(),
        ));

        tracing::debug!(sock = %sock_path.display(), "codex app-server: connected");

        let client = Self {
            sink,
            transport,
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

    /// `thread/unsubscribe` — drop THIS connection's subscription; codex unloads the thread
    /// (and its MCP servers) once its last subscriber leaves. A later `thread/resume` reloads it.
    pub async fn thread_unsubscribe(
        &self,
        thread_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ThreadUnsubscribeResponse> {
        self.request_until(
            "thread/unsubscribe",
            json!({ "threadId": thread_id }),
            deadline,
        )
        .await
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

    /// `turn/steer` — push more input into the running turn. Codex refuses with `-32600` when no turn or a different turn is active (reaches the caller as [`Error::Refused`]).
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

    /// `thread/revert {threadId, beforeTurnId}` — replace the thread's durable history with the
    /// prefix before `before_turn_id`. Local file changes are not reverted. The vendored protocol
    /// calls this `thread/rollback`, which the pinned binary rejects.
    pub async fn thread_revert(&self, thread_id: &str, before_turn_id: &str) -> Result<()> {
        thread_revert_outcome(
            self.request(
                "thread/revert",
                json!({ "threadId": thread_id, "beforeTurnId": before_turn_id }),
            )
            .await,
        )
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
            Err(Error::Refused(message))
                if message
                    == "turn/interrupt failed: no active turn to interrupt (code -32600)" =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Core round-trip. A JSON-RPC `error` frame maps to [`Error::Refused`]; transport failures and a dead reader map to [`Error::Transport`].
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
            return Err(Error::Transport(format!(
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
                return Err(Error::Transport(format!("send {method}: {e}")));
            }
            sending.complete();
        }

        tracing::trace!(id, method, "codex app-server: request sent");

        // On elapse remove our own pending entry so the map does not leak the never-fired oneshot. This timer is not a turn lifecycle criterion.
        let outcome = match tokio::time::timeout_at(deadline, rx).await {
            Ok(received) => received,
            Err(_elapsed) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(Error::Transport(format!("request {method} timed out")));
            }
        };

        match outcome {
            Ok(Ok(value)) => serde_json::from_value(value)
                .map_err(|e| Error::Transport(format!("decode {method} result: {e}"))),
            // The one place codex's own refusal is still distinguishable from everything else that can go wrong.
            Ok(Err(rpc)) => Err(Error::Refused(format!(
                "{method} failed: {} (code {})",
                rpc.message, rpc.code
            ))),
            // Sender dropped without sending: the reader task ended before our response arrived.
            Err(_) => Err(Error::Transport(format!(
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
) {
    let mut requests = server_requests::Dispatch::new(sink, transport);
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
mod tests;

pub fn thread_id_from_started(params: &serde_json::Value) -> Option<&str> {
    if let Some(id) = params
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(id);
    }
    params.get("threadId").and_then(serde_json::Value::as_str)
}

pub fn other_thread_id(params: &serde_json::Value) -> Option<&str> {
    params.get("threadId").and_then(serde_json::Value::as_str)
}

#[cfg(test)]
mod thread_revert_wire_tests;
#[cfg(test)]
mod thread_start_wire_tests;
