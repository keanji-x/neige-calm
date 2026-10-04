//! Server-initiated requests. Neige offers Codex no dynamic tool and serves no server request:
//! each one is refused with an explicit JSON-RPC error, so a thread that still carries a
//! since-deleted dynamic tool gets an answer when it calls it.
//! A saturated reply queue closes the socket: a peer that cannot receive an error must not cause
//! unbounded tasks or memory.
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

/// Owned by the reader. Dropping it cancels the writer.
pub(super) struct Dispatch {
    pub(super) tasks: JoinSet<bool>,
    replies: mpsc::Sender<Value>,
    transport: Arc<TransportAbort>,
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        self.transport.poison();
    }
}

impl Dispatch {
    pub(super) fn new(sink: WsSink, transport: Arc<TransportAbort>) -> Self {
        let (replies, mut rx) = mpsc::channel::<Value>(REPLY_LIMIT);
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
            replies,
            transport,
        }
    }

    /// Synchronous admission only: socket flushing never blocks the reader's RPC response and
    /// notification routing.
    pub(super) fn accept(&mut self, frame: &Value) -> bool {
        let (id, code, message) =
            match serde_json::from_value::<ServerRequestId>(frame["id"].clone()) {
                Ok(id) => (json!(id), -32601, "unsupported server request"),
                Err(_) => (Value::Null, -32600, "invalid server request ID"),
            };
        let reply = json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
        if self.replies.try_send(reply).is_err() {
            self.transport.poison();
            return false;
        }
        true
    }
}
