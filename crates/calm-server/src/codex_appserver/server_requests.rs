//! Connection-owned dynamic-tool requests. No business authority is inferred here.
//! There are at most 16 outstanding handlers, 16 queued deliveries, and 32 queued
//! replies. A saturated reply queue or duplicate in-flight ID closes the socket:
//! a peer that cannot receive an error must not cause unbounded tasks or memory.
use super::*;
use std::collections::HashSet;
use tokio::task::JoinSet;

const HANDLER_LIMIT: usize = 16;
const REPLY_LIMIT: usize = 32;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Preserve the provider's JSON-RPC ID independently of client-generated IDs.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum ServerRequestId {
    String(String),
    Integer(i64),
    Unsigned(u64),
}

/// Provider envelope, not tool arguments. The application must still authenticate
/// thread/session and authorize any effects; these fields confer no authority.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallParams {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub namespace: Option<String>,
    pub arguments: Value,
}

/// F2-A only needs text results; other content types can be added when used.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallResponse {
    pub success: bool,
    pub content_items: Vec<DynamicToolText>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub enum DynamicToolText {
    #[serde(rename = "inputText")]
    Text { text: String },
}

impl DynamicToolCallResponse {
    pub fn text(success: bool, text: impl Into<String>) -> Self {
        Self {
            success,
            content_items: vec![DynamicToolText::Text { text: text.into() }],
        }
    }
}

/// Single-owner delivery. Reply ownership is tied to this connection, never a
/// daemon's replaceable current client. Timeout/drop cancels the response wait;
/// it cannot undo application effects already committed by a consumer.
#[derive(Debug)]
pub struct DynamicToolRequest {
    pub id: ServerRequestId,
    pub params: DynamicToolCallParams,
    response: oneshot::Sender<DynamicToolCallResponse>,
}

impl DynamicToolRequest {
    pub fn is_cancelled(&self) -> bool {
        self.response.is_closed()
    }

    pub fn respond(self, response: DynamicToolCallResponse) -> Result<()> {
        self.response.send(response).map_err(|_| {
            CalmError::CodexAppServer(
                "dynamic tool request expired or its connection closed".into(),
            )
        })
    }
}

#[derive(Default)]
pub(super) struct Registration {
    state: StdMutex<RegistrationState>,
}

#[derive(Default)]
enum RegistrationState {
    #[default]
    Unregistered,
    Registered(mpsc::Sender<DynamicToolRequest>),
    Closed,
}

impl Registration {
    pub(super) fn take(&self) -> Result<mpsc::Receiver<DynamicToolRequest>> {
        let mut slot = self.state.lock().expect("dynamic tool registration");
        match &*slot {
            RegistrationState::Unregistered => {}
            RegistrationState::Registered(_) => {
                return Err(CalmError::CodexAppServer(
                    "dynamic tool receiver already registered on this connection".into(),
                ));
            }
            RegistrationState::Closed => {
                return Err(CalmError::CodexAppServer(
                    "dynamic tool connection is closed".into(),
                ));
            }
        }
        let (tx, rx) = mpsc::channel(HANDLER_LIMIT);
        *slot = RegistrationState::Registered(tx);
        Ok(rx)
    }

    fn sender(&self) -> Option<mpsc::Sender<DynamicToolRequest>> {
        match &*self.state.lock().expect("dynamic tool registration") {
            RegistrationState::Registered(sender) => Some(sender.clone()),
            RegistrationState::Unregistered | RegistrationState::Closed => None,
        }
    }

    pub(super) fn close(&self) {
        // Drop the stored sender even while a public client retains this state.
        // Closed is permanent: a late registration cannot create an inert queue.
        *self.state.lock().expect("dynamic tool registration") = RegistrationState::Closed;
    }
}

struct Reply {
    id: Option<ServerRequestId>,
    value: Value,
}

fn error(id: Option<ServerRequestId>, code: i64, message: &str) -> Reply {
    Reply {
        value: json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
        id,
    }
}

/// Owned by the reader. Dropping it cancels all response waits and the writer.
/// No worker holds a strong CodexAppServer or can migrate to a new connection.
pub(super) struct Dispatch {
    pub(super) tasks: JoinSet<bool>,
    replies: mpsc::Sender<Reply>,
    active: Arc<StdMutex<HashSet<ServerRequestId>>>,
    registration: Arc<Registration>,
    transport: Arc<TransportAbort>,
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        self.registration.close();
        self.transport.poison();
    }
}

impl Dispatch {
    pub(super) fn new(
        sink: WsSink,
        transport: Arc<TransportAbort>,
        registration: Arc<Registration>,
    ) -> Self {
        let (replies, mut rx) = mpsc::channel::<Reply>(REPLY_LIMIT);
        let active = Arc::new(StdMutex::new(HashSet::new()));
        let mut tasks = JoinSet::new();
        let writer_transport = transport.clone();
        let writer_active = active.clone();
        tasks.spawn(async move {
            while let Some(reply) = rx.recv().await {
                let sent = tokio::time::timeout(WRITE_TIMEOUT, async {
                    let mut writer = sink.lock().await;
                    let mut sending = writer_transport.sending()?;
                    writer
                        .send(Message::Text(reply.value.to_string()))
                        .await
                        .map_err(|error| {
                            CalmError::CodexAppServer(format!("server reply write: {error}"))
                        })?;
                    sending.complete();
                    Ok::<_, CalmError>(())
                })
                .await;
                if !matches!(sent, Ok(Ok(()))) {
                    writer_transport.poison();
                    return false;
                }
                if let Some(id) = reply.id {
                    writer_active
                        .lock()
                        .expect("server request IDs")
                        .remove(&id);
                }
            }
            false
        });
        Self {
            tasks,
            replies,
            active,
            registration,
            transport,
        }
    }

    fn reply(&self, reply: Reply) -> bool {
        if self.replies.try_send(reply).is_err() {
            self.transport.poison();
            return false;
        }
        true
    }

    /// Synchronous admission only. Neither handler latency nor socket flushing
    /// blocks the reader's RPC response/notification routing.
    pub(super) fn accept(&mut self, frame: &Value) -> bool {
        let id = match serde_json::from_value::<ServerRequestId>(frame["id"].clone()) {
            Ok(id) => id,
            Err(_) => return self.reply(error(None, -32600, "invalid server request ID")),
        };
        if !self
            .active
            .lock()
            .expect("server request IDs")
            .insert(id.clone())
        {
            self.transport.poison();
            return false;
        }
        if frame["method"] != "item/tool/call" {
            return self.reply(error(Some(id), -32601, "unsupported server request method"));
        }
        if frame["params"].get("arguments").is_none() {
            return self.reply(error(Some(id), -32602, "missing dynamic tool arguments"));
        }
        let params = match serde_json::from_value::<DynamicToolCallParams>(frame["params"].clone())
        {
            Ok(params) => params,
            Err(_) => {
                return self.reply(error(Some(id), -32602, "invalid dynamic tool parameters"));
            }
        };
        if params.thread_id.is_empty()
            || params.turn_id.is_empty()
            || params.call_id.is_empty()
            || params.tool.is_empty()
        {
            return self.reply(error(
                Some(id),
                -32602,
                "dynamic tool identity fields must be nonempty",
            ));
        }
        let handler = self.registration.sender();
        let Some(handler) = handler else {
            return self.reply(error(
                Some(id),
                -32601,
                "no dynamic tool handler registered",
            ));
        };
        // One task is the writer. Completed tasks are reaped by the reader.
        if self.tasks.len() > HANDLER_LIMIT {
            return self.reply(error(
                Some(id),
                -32000,
                "dynamic tool handler capacity exceeded",
            ));
        }
        let (response, result) = oneshot::channel();
        let request = DynamicToolRequest {
            id: id.clone(),
            params,
            response,
        };
        if let Err(error_kind) = handler.try_send(request) {
            let message = if matches!(error_kind, mpsc::error::TrySendError::Closed(_)) {
                "dynamic tool handler unavailable"
            } else {
                "dynamic tool delivery queue full"
            };
            return self.reply(error(Some(id), -32000, message));
        }
        let replies = self.replies.clone();
        let transport = self.transport.clone();
        self.tasks.spawn(async move {
            let reply = match tokio::time::timeout(HANDLER_TIMEOUT, result).await {
                Ok(Ok(response)) => Reply {
                    value: json!({"jsonrpc":"2.0","id":id,"result":response}),
                    id: Some(id),
                },
                Ok(Err(_)) => error(Some(id), -32603, "dynamic tool handler dropped request"),
                Err(_) => error(
                    Some(id),
                    -32001,
                    "dynamic tool handler timed out; outcome may be unknown",
                ),
            };
            if replies.try_send(reply).is_err() {
                transport.poison();
                return false;
            }
            true
        });
        true
    }
}
