use crate::send_boundary::PreSendGate;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

const INCOMING_LIMIT: usize = 256;
const FRAME_LIMIT: usize = 8 * 1024 * 1024;
const PENDING_LIMIT: usize = 64;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);
const TRANSPORT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("ACP connection closed; dispatched request outcomes may be unknown")]
    Closed,
    #[error("ACP peer output reached EOF")]
    Eof,
    #[error("ACP local operation abandoned: {0}")]
    Abandoned(&'static str),
    #[error("ACP {operation} failed ({kind:?})")]
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    #[error("ACP admission refused: {0}")]
    Refused(String),
    #[error("ACP protocol violation: {0}")]
    Protocol(&'static str),
    #[error("ACP request timed out; its outcome is unknown")]
    Timeout,
    #[error("ACP peer returned RPC error {code}")]
    Remote {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}

#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

impl Incoming {
    fn session_id(&self) -> Option<&str> {
        let params = match self {
            Self::Notification { params, .. } | Self::Request { params, .. } => params,
        };
        params.get("sessionId")?.as_str()
    }
}

struct SessionRoute {
    sender: mpsc::Sender<Incoming>,
    overflow: watch::Sender<bool>,
}
type SessionRoutes = Arc<SyncMutex<HashMap<String, SessionRoute>>>;

/// Exclusive inbound stream for one native session on a shared connection.
pub struct SessionIncoming {
    session_id: String,
    routes: SessionRoutes,
    receiver: mpsc::Receiver<Incoming>,
    overflow: watch::Receiver<bool>,
}
impl SessionIncoming {
    pub fn overflow(&self) -> watch::Receiver<bool> {
        self.overflow.clone()
    }

    pub fn receiver_mut(&mut self) -> &mut mpsc::Receiver<Incoming> {
        &mut self.receiver
    }

    pub async fn recv(&mut self) -> Option<Incoming> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Incoming, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}
impl Drop for SessionIncoming {
    fn drop(&mut self) {
        self.routes
            .lock()
            .expect("ACP routes")
            .remove(&self.session_id);
    }
}

type Answer = oneshot::Sender<Result<Value, Error>>;
/// Factual transport failure. Crossing does not establish native acceptance.
#[derive(Debug, thiserror::Error)]
pub enum WriteFailure {
    #[error("ACP refused before write: {0}")]
    BeforeWrite(Error),
    #[error("ACP write crossed: {0}")]
    CrossedWrite(Error),
}
impl WriteFailure {
    pub fn into_error(self) -> Error {
        match self {
            Self::BeforeWrite(error) => error,
            Self::CrossedWrite(error) => error,
        }
    }
}
struct Registration {
    id: u64,
    pending: RegisteredResponse,
    registered: oneshot::Sender<Result<(), Error>>,
}
struct RegisteredResponse {
    answer: Answer,
    // A scoped request that reached the wire must keep its ID until the peer
    // responds, even if its owner no longer waits.
    retain_late: Option<Arc<AtomicBool>>,
    prompt: bool,
}
type Writer = Box<dyn AsyncWrite + Unpin + Send>;
struct Inner {
    registrations: mpsc::Sender<Registration>,
    writer: Arc<Mutex<Option<Writer>>>,
    close: watch::Sender<Option<Error>>,
    next_id: AtomicU64,
    shared: bool,
    overloaded: Arc<AtomicBool>,
    retiring: Arc<AtomicBool>,
    routes: SessionRoutes,
}
impl Drop for Inner {
    fn drop(&mut self) {
        record_failure(&self.close, Error::Closed);
    }
}
#[derive(Clone)]
pub struct Client(Arc<Inner>);

/// One stream owner. Consume incoming events concurrently with pending RPC responses.
pub struct Connection {
    pub client: Client,
    pub incoming: mpsc::Receiver<Incoming>,
}

/// A request fully written to the pipe. This is dispatch evidence, not native acceptance.
pub struct PendingResponse {
    response: oneshot::Receiver<Result<Value, Error>>,
    close: watch::Sender<Option<Error>>,
    settled: bool,
    scoped: bool,
}
impl Drop for PendingResponse {
    fn drop(&mut self) {
        if !self.settled && !self.scoped {
            record_failure(&self.close, Error::Abandoned("response wait"));
        }
    }
}
impl PendingResponse {
    pub async fn wait(mut self, timeout: Duration) -> Result<Value, Error> {
        let result = match tokio::time::timeout(timeout, &mut self.response).await {
            Ok(answer) => {
                answer.unwrap_or_else(|_| Err(self.close.borrow().clone().unwrap_or(Error::Closed)))
            }
            Err(_) if self.scoped => Err(Error::Timeout),
            Err(_) => {
                record_failure(&self.close, Error::Timeout);
                Err(self.close.borrow().clone().expect("timeout published"))
            }
        };
        self.settled = true;
        result
    }
}
/// The same wire-size check serves preflight and the final write boundary.
fn frame_bytes(frame: &Value) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(frame).map_err(|_| Error::Protocol("cannot encode frame"))?;
    if bytes.len() >= FRAME_LIMIT {
        return Err(Error::Protocol("outgoing frame exceeds byte limit"));
    }
    Ok(bytes)
}

impl Client {
    /// Validate an outgoing request without reserving an id or touching the connection.
    /// The widest request id conservatively covers every id the writer can allocate.
    pub fn validate_request(method: &str, params: Value) -> Result<(), Error> {
        frame_bytes(&json!({"jsonrpc":"2.0", "id":u64::MAX, "method":method, "params":params}))
            .map(drop)
    }

    pub fn is_shared(&self) -> bool {
        self.0.shared
    }

    pub fn retire_shared_when_idle(&self) {
        self.0.retiring.store(true, Ordering::Release);
    }

    pub fn shared_retiring(&self) -> bool {
        self.0.retiring.load(Ordering::Acquire)
    }

    /// A saturated or quarantined generation can be retired without interrupting any routed turn.
    pub fn can_recycle_shared(&self) -> bool {
        self.0.shared
            && (self.shared_retiring() || self.0.overloaded.load(Ordering::Acquire))
            && self.0.routes.lock().expect("ACP routes").is_empty()
    }

    /// Route subsequent frames for an already identified native session to its owner.
    /// Frames without a registered session still go to the connection owner, including
    /// setup traffic for a session whose native ID has not been returned yet.
    pub fn route_session(&self, session_id: &str) -> Result<SessionIncoming, Error> {
        let mut routes = self.0.routes.lock().expect("ACP routes");
        if let Some(error) = self.terminal_error() {
            return Err(error);
        }
        if session_id.is_empty() || routes.contains_key(session_id) {
            return Err(Error::Protocol("invalid or duplicate session route"));
        }
        let (sender, receiver) = mpsc::channel(INCOMING_LIMIT);
        let (overflow, overflow_rx) = watch::channel(false);
        routes.insert(session_id.to_owned(), SessionRoute { sender, overflow });
        Ok(SessionIncoming {
            session_id: session_id.to_owned(),
            routes: self.0.routes.clone(),
            receiver,
            overflow: overflow_rx,
        })
    }

    pub async fn submit_gated(
        &self,
        method: &str,
        params: Value,
        gate: &PreSendGate<'_>,
    ) -> Result<PendingResponse, WriteFailure> {
        let scoped = self.0.shared
            || params["sessionId"]
                .as_str()
                .is_some_and(|id| self.0.routes.lock().expect("ACP routes").contains_key(id));
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, response) = oneshot::channel();
        self.write_gated(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            Some((
                id,
                RegisteredResponse {
                    answer,
                    retain_late: scoped.then(|| Arc::new(AtomicBool::new(false))),
                    prompt: method == "session/prompt",
                },
            )),
            gate,
        )
        .await?;
        Ok(PendingResponse {
            response,
            close: self.0.close.clone(),
            settled: false,
            scoped,
        })
    }
    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        self.submit_gated(method, params, &allow())
            .await
            .map_err(WriteFailure::into_error)?
            .wait(timeout)
            .await
    }
    pub async fn respond(&self, id: Value, result: Value) -> Result<(), Error> {
        self.write_reply(json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await
    }
    pub async fn reject_method(&self, id: Value) -> Result<(), Error> {
        self.write_reply(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client method"}})).await
    }
    /// The transport owner's first terminal error, published before either consumer closes.
    /// Diagnostic evidence only: this never proves whether a remote prompt executed.
    pub fn terminal_error(&self) -> Option<Error> {
        self.0.close.borrow().clone()
    }
    fn closed_error(&self) -> Error {
        self.terminal_error().unwrap_or(Error::Closed)
    }
    pub fn close(&self) {
        record_failure(&self.0.close, Error::Closed);
    }
    pub async fn notify_gated(
        &self,
        method: &str,
        params: Value,
        gate: &PreSendGate<'_>,
    ) -> Result<(), WriteFailure> {
        self.write_gated(
            json!({"jsonrpc":"2.0","method":method,"params":params}),
            None,
            gate,
        )
        .await
    }
    async fn write_reply(&self, frame: Value) -> Result<(), Error> {
        // An exclusive connection can fence on any abandoned reply. Shared
        // reply owners quarantine the affected session after a pre-write
        // failure; write_gated still fences partial frames in either mode.
        let mut failure = CloseOnDrop {
            close: &self.0.close,
            armed: !self.0.shared,
        };
        self.write_gated(frame, None, &allow())
            .await
            .map_err(WriteFailure::into_error)?;
        failure.armed = false;
        Ok(())
    }

    async fn write_gated(
        &self,
        frame: Value,
        response: Option<(u64, RegisteredResponse)>,
        gate: &PreSendGate<'_>,
    ) -> Result<(), WriteFailure> {
        let before = |error: String| WriteFailure::BeforeWrite(Error::Refused(error));
        let prompt_session = (frame["method"] == "session/prompt")
            .then(|| frame["params"]["sessionId"].as_str())
            .flatten();
        let mut bytes = frame_bytes(&frame).map_err(|error| {
            if !self.0.shared {
                record_failure(&self.0.close, error.clone());
            }
            WriteFailure::BeforeWrite(error)
        })?;
        bytes.push(b'\n');
        let deadline = tokio::time::Instant::now() + TRANSPORT_WAIT_TIMEOUT;
        let mut guard = tokio::time::timeout_at(deadline, self.0.writer.lock())
            .await
            .map_err(|_| before("writer wait timed out".into()))?;
        if self.0.close.borrow().is_some() || guard.is_none() {
            return Err(WriteFailure::BeforeWrite(self.closed_error()));
        }
        // The incoming owner registers before any possible write and preserves the
        // pending bound. This queue carries no frames or permission decisions.
        let retain_late = response
            .as_ref()
            .and_then(|(_, response)| response.retain_late.clone());
        if let Some((id, pending)) = response {
            let (registered, receipt) = oneshot::channel();
            tokio::time::timeout_at(deadline, async {
                self.0
                    .registrations
                    .send(Registration {
                        id,
                        pending,
                        registered,
                    })
                    .await
                    .map_err(|_| self.closed_error())?;
                receipt.await.unwrap_or_else(|_| Err(self.closed_error()))
            })
            .await
            .map_err(|_| before("response registration timed out".into()))?
            .map_err(WriteFailure::BeforeWrite)?;
        }
        // Construct all I/O machinery before admission. The gate's final synchronous
        // check and the first write poll occur in the same task poll, with no await
        // on another owner between them.
        let mut crossing = CloseOnDrop {
            close: &self.0.close,
            armed: false,
        };
        let mut closed = self.0.close.subscribe();
        let write = async {
            if closed.borrow().is_some() {
                return Err(WriteFailure::BeforeWrite(self.closed_error()));
            }
            if self.0.shared
                && let Some(session) = prompt_session
            {
                if self.shared_retiring() {
                    return Err(before("shared ACP generation is retiring".into()));
                }
                if self
                    .0
                    .routes
                    .lock()
                    .expect("ACP routes")
                    .get(session)
                    .is_none_or(|route| *route.overflow.borrow())
                {
                    return Err(before("shared ACP prompt route is unavailable".into()));
                }
            }
            if let Some(retain) = retain_late {
                retain.store(true, Ordering::Release);
            }
            crossing.armed = true;
            let mut writer = guard.take().expect("writer owned under lock");
            let outcome = {
                let io = tokio::time::timeout(WRITE_TIMEOUT, async {
                    writer.write_all(&bytes).await?;
                    writer.flush().await
                });
                let closing = closed.changed();
                tokio::pin!(io, closing);
                // Tokio select cooperatively yields before polling its branches.
                // Poll these futures directly so the final check is immediately
                // followed by the first AsyncWrite poll, even with no task budget.
                match futures_util::future::select(closing, io).await {
                    futures_util::future::Either::Left(_) => Err(self.closed_error()),
                    futures_util::future::Either::Right((result, _)) => match result {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(Error::Io {
                            operation: "write",
                            kind: error.kind(),
                        }),
                        Err(_) => Err(Error::Timeout),
                    },
                }
            };
            if let Err(error) = outcome {
                record_failure(&self.0.close, error.clone());
                return Err(WriteFailure::CrossedWrite(error));
            }
            *guard = Some(writer);
            crossing.armed = false;
            Ok(())
        };
        tokio::time::timeout(ADMISSION_TIMEOUT, gate.run())
            .await
            .map_err(|_| before("admission timed out".into()))?
            .map_err(|error| before(error.0))?;
        write.await
    }
}
struct CloseOnDrop<'a> {
    close: &'a watch::Sender<Option<Error>>,
    armed: bool,
}
impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            record_failure(self.close, Error::Abandoned("reply or crossed write"));
        }
    }
}
fn record_failure(close: &watch::Sender<Option<Error>>, error: Error) {
    close.send_if_modified(|current| {
        if current.is_none() {
            *current = Some(error);
            true
        } else {
            false
        }
    });
}
fn allow() -> PreSendGate<'static> {
    PreSendGate::new(|| Box::pin(async { Ok(()) }))
}

impl Connection {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::with_scope(reader, writer, false)
    }

    pub fn new_shared<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::with_scope(reader, writer, true)
    }

    fn with_scope<R, W>(reader: R, writer: W, shared: bool) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (registrations, registration_rx) = mpsc::channel(64);
        let writer = Arc::new(Mutex::new(Some(Box::new(writer) as Writer)));
        let (incoming, incoming_rx) = mpsc::channel(INCOMING_LIMIT);
        let routes = SessionRoutes::default();
        let (close, close_rx) = watch::channel(None);
        let overloaded = Arc::new(AtomicBool::new(false));
        let retiring = Arc::new(AtomicBool::new(false));
        let client = Client(Arc::new(Inner {
            registrations,
            writer: writer.clone(),
            close: close.clone(),
            next_id: AtomicU64::new(1),
            shared,
            overloaded: overloaded.clone(),
            retiring: retiring.clone(),
            routes: routes.clone(),
        }));
        tokio::spawn(run(
            reader,
            writer,
            registration_rx,
            incoming,
            routes,
            shared,
            overloaded,
            retiring,
            close,
            close_rx,
        ));
        Self {
            client,
            incoming: incoming_rx,
        }
    }
}
struct ReaderGuard(tokio::task::JoinHandle<()>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[allow(clippy::too_many_arguments)]
async fn run<R>(
    reader: R,
    writer: Arc<Mutex<Option<Writer>>>,
    mut registrations: mpsc::Receiver<Registration>,
    incoming: mpsc::Sender<Incoming>,
    routes: SessionRoutes,
    shared: bool,
    overloaded: Arc<AtomicBool>,
    retiring: Arc<AtomicBool>,
    close: watch::Sender<Option<Error>>,
    mut closed: watch::Receiver<Option<Error>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (frames, mut frame_rx) = mpsc::channel(16);
    let _reader = ReaderGuard(tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        loop {
            let mut bytes = Vec::new();
            let frame = match (&mut reader)
                .take((FRAME_LIMIT + 1) as u64)
                .read_until(b'\n', &mut bytes)
                .await
            {
                Ok(0) => Err(Error::Eof),
                Err(error) => Err(Error::Io {
                    operation: "read",
                    kind: error.kind(),
                }),
                Ok(_) if bytes.len() > FRAME_LIMIT => {
                    Err(Error::Protocol("frame exceeds byte limit"))
                }
                Ok(_) if !bytes.ends_with(b"\n") => Err(Error::Protocol("truncated frame")),
                Ok(_) => serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Protocol("invalid JSON frame")),
            };
            let ended = frame.is_err();
            if frames.send(frame).await.is_err() || ended {
                break;
            }
        }
    }));
    let mut pending = HashMap::<u64, RegisteredResponse>::new();
    let error = loop {
        tokio::select! {
            biased;
            _ = closed.changed() => break closed.borrow().clone().unwrap_or(Error::Closed),
            registration = registrations.recv() => {
                let Some(registration) = registration else { break Error::Closed; };
                pending.retain(|_, response| !response.answer.is_closed() || response.retain_late.as_ref().is_some_and(|retain| retain.load(Ordering::Acquire)));
                if registration.registered.is_closed() { continue; }
                let limit = if shared && registration.pending.prompt {
                    PENDING_LIMIT - 8
                } else {
                    PENDING_LIMIT
                };
                if pending.len() >= limit {
                    overloaded.store(true, Ordering::Release);
                    let error = Error::Protocol("too many pending requests");
                    let _ = registration.registered.send(Err(error.clone()));
                    if shared || registration.pending.retain_late.is_some() {
                        continue;
                    }
                    break error;
                }
                pending.insert(registration.id, registration.pending);
                if registration.registered.send(Ok(())).is_err() {
                    pending.remove(&registration.id);
                }
            }
            frame = frame_rx.recv() => {
                let frame = match frame { Some(Ok(frame)) => frame, Some(Err(error)) => break error, None => break Error::Closed };
                if let Err(error) = receive(frame, &mut pending, &incoming, &routes, shared.then_some(retiring.as_ref())) { break error; }
                if pending.len() < PENDING_LIMIT - 8 {
                    overloaded.store(false, Ordering::Release);
                }
            }
        }
    };
    record_failure(&close, error);
    routes.lock().expect("ACP routes").clear();
    let error = close.borrow().clone().expect("terminal error published");
    tracing::debug!(error = %error, pending_requests = pending.len(), "ACP transport stopped");
    for (_, answer) in pending {
        let _ = answer.answer.send(Err(error.clone()));
    }
    // The caller observes close while holding the writer, then releases it.
    writer.lock().await.take();
}
fn receive(
    frame: Value,
    pending: &mut HashMap<u64, RegisteredResponse>,
    incoming: &mpsc::Sender<Incoming>,
    routes: &SessionRoutes,
    retiring: Option<&AtomicBool>,
) -> Result<(), Error> {
    if !frame.is_object() || frame["jsonrpc"] != "2.0" {
        return Err(Error::Protocol("invalid JSON-RPC envelope"));
    }
    if let Some(method) = frame.get("method") {
        let method = method
            .as_str()
            .ok_or(Error::Protocol("invalid method"))?
            .to_owned();
        if frame.get("result").is_some() || frame.get("error").is_some() {
            return Err(Error::Protocol("request contains response fields"));
        }
        let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
        let message = match frame.get("id") {
            Some(id) if valid_id(id) => Incoming::Request {
                id: id.clone(),
                method,
                params,
            },
            Some(_) => return Err(Error::Protocol("invalid request id")),
            None => Incoming::Notification { method, params },
        };
        let routes = routes.lock().expect("ACP routes");
        if let Some(route) = message.session_id().and_then(|session| routes.get(session)) {
            match route.sender.try_send(message) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    route.overflow.send_replace(true);
                    // Quarantine closes this native session, including any request
                    // whose answer can no longer be associated with a live turn.
                    return Ok(());
                }
            }
        }
        let notification = matches!(message, Incoming::Notification { .. });
        return incoming
            .try_send(message)
            .or_else(|error| {
                if let Some(retiring) = retiring
                    && matches!(error, mpsc::error::TrySendError::Full(_))
                {
                    // A shared setup or orphan request overflow cannot be assigned
                    // to a live turn. Drain and recycle after routed turns finish.
                    if !notification {
                        retiring.store(true, Ordering::Release);
                    }
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => Error::Protocol("incoming event queue full"),
                mpsc::error::TrySendError::Closed(_) => {
                    Error::Protocol("incoming event consumer dropped")
                }
            });
    }
    let id = frame["id"]
        .as_u64()
        .ok_or(Error::Protocol("invalid response id"))?;
    let response = match (frame.get("result"), frame.get("error")) {
        (Some(value), None) => Ok(value.clone()),
        (None, Some(error)) => Err(Error::Remote {
            code: error["code"]
                .as_i64()
                .ok_or(Error::Protocol("invalid error code"))?,
            message: error["message"]
                .as_str()
                .ok_or(Error::Protocol("invalid error message"))?
                .into(),
            data: error.get("data").cloned(),
        }),
        _ => {
            return Err(Error::Protocol(
                "response needs exactly one result or error",
            ));
        }
    };
    let answer = pending
        .remove(&id)
        .ok_or(Error::Protocol("unsolicited or duplicate response"))?;
    let _ = answer.answer.send(response);
    Ok(())
}
fn valid_id(value: &Value) -> bool {
    value.is_string() || value.as_i64().is_some() || value.as_u64().is_some()
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod write_tests;
