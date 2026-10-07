//! Server-initiated requests. Neige offers Codex no dynamic tool and serves no server request but
//! one kind: an approval on a thread a Planner harness routes ([`ApprovalRoutes`], #2348) goes to
//! that harness, which answers it later. Every other request is refused with an explicit JSON-RPC
//! error, so a thread that still carries a since-deleted dynamic tool gets an answer when it calls
//! it. A saturated reply queue closes the socket: a peer that cannot receive an answer must not
//! cause unbounded tasks or memory.
use super::approvals::{ApprovalRequest, ApprovalResponder, ApprovalRoutes, HandedRequests};
use super::*;
use tokio::task::JoinSet;

const REPLY_LIMIT: usize = 32;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Echo the provider's JSON-RPC ID in its own spelling, independently of client-generated IDs.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
enum ServerRequestId {
    String(String),
    Integer(i64),
    Unsigned(u64),
}

/// The connection's reply queue. Queuing never waits: a full queue closes the socket.
#[derive(Clone)]
struct Replies {
    queue: mpsc::Sender<Value>,
    transport: Arc<TransportAbort>,
}

impl Replies {
    fn send(&self, reply: Value) -> bool {
        if self.queue.try_send(reply).is_err() {
            self.transport.poison();
            return false;
        }
        true
    }
}

impl approvals::ReplyPort for Replies {
    fn reply(&self, frame: Value) {
        self.send(frame);
    }
}

/// Owned by the reader. Dropping it cancels the writer and tells every harness it handed a
/// request that the connection is gone.
pub(super) struct Dispatch {
    pub(super) tasks: JoinSet<bool>,
    replies: Replies,
    routes: Arc<ApprovalRoutes>,
    handed: HandedRequests,
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        self.replies.transport.poison();
    }
}

impl Dispatch {
    pub(super) fn new(
        sink: WsSink,
        transport: Arc<TransportAbort>,
        routes: Arc<ApprovalRoutes>,
    ) -> Self {
        let (queue, mut rx) = mpsc::channel::<Value>(REPLY_LIMIT);
        let mut tasks = JoinSet::new();
        let writer_transport = transport.clone();
        tasks.spawn(async move {
            while let Some(reply) = rx.recv().await {
                let sent = tokio::time::timeout(WRITE_TIMEOUT, async {
                    let mut writer = sink.lock().await;
                    let mut sending = writer_transport.sending()?;
                    writer
                        .send(Message::Text(reply.to_string()))
                        .await
                        .map_err(|error| {
                            Error::Transport(format!("server reply write: {error}"))
                        })?;
                    sending.complete();
                    Ok::<_, Error>(())
                })
                .await;
                if !matches!(sent, Ok(Ok(()))) {
                    writer_transport.poison();
                    return false;
                }
            }
            false
        });
        Self {
            tasks,
            replies: Replies { queue, transport },
            routes,
            handed: HandedRequests::new(),
        }
    }

    /// Synchronous admission only: socket flushing never blocks the reader's RPC response and
    /// notification routing, and handing an approval to its harness never waits.
    pub(super) fn accept(&mut self, frame: &Value) -> bool {
        if serde_json::from_value::<ServerRequestId>(frame["id"].clone()).is_err() {
            return self.refuse(Value::Null, -32600, "invalid server request ID");
        }
        let id = &frame["id"];
        let method = frame["method"].as_str().unwrap_or_default();
        if let Some(request) = ApprovalRequest::parse(method, &frame["params"])
            && let Some(sender) = self.routes.sender(&request.thread_id)
        {
            let responder = ApprovalResponder::new(self.replies.clone(), id.clone(), request.kind);
            self.handed.open(sender, id, &request, Box::new(responder));
            return true;
        }
        self.refuse(id.clone(), -32601, "unsupported server request")
    }

    /// `serverRequest/resolved`: codex settled one of its requests.
    pub(super) fn resolved(&mut self, params: &Value) {
        self.handed.resolved(params);
    }

    fn refuse(&self, id: Value, code: i64, message: &str) -> bool {
        self.replies
            .send(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}))
    }
}
