use crate::send_boundary::PreSendGate;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

const FRAME_LIMIT: usize = 8 * 1024 * 1024;
const PENDING_LIMIT: usize = 64;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("ACP connection closed; dispatched request outcomes may be unknown")]
    Closed,
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
    answer: Answer,
    registered: oneshot::Sender<Result<(), Error>>,
}
type Writer = Box<dyn AsyncWrite + Unpin + Send>;
struct Inner {
    registrations: mpsc::Sender<Registration>,
    writer: Arc<Mutex<Option<Writer>>>,
    close: watch::Sender<bool>,
    next_id: AtomicU64,
}
impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.close.send(true);
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
    close: watch::Sender<bool>,
    settled: bool,
}
impl Drop for PendingResponse {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.close.send(true);
        }
    }
}
impl PendingResponse {
    pub async fn wait(mut self, timeout: Duration) -> Result<Value, Error> {
        let result = match tokio::time::timeout(timeout, &mut self.response).await {
            Ok(answer) => answer.unwrap_or(Err(Error::Closed)),
            Err(_) => {
                let _ = self.close.send(true);
                Err(Error::Timeout)
            }
        };
        self.settled = true;
        result
    }
}
impl Client {
    pub async fn submit_gated(
        &self,
        method: &str,
        params: Value,
        gate: &PreSendGate<'_>,
    ) -> Result<PendingResponse, WriteFailure> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, response) = oneshot::channel();
        self.write_gated(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            Some((id, answer)),
            gate,
        )
        .await?;
        Ok(PendingResponse {
            response,
            close: self.0.close.clone(),
            settled: false,
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
        self.write(json!({"jsonrpc":"2.0","id":id,"result":result}), None)
            .await
    }
    pub async fn reject_method(&self, id: Value) -> Result<(), Error> {
        self.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client method"}}), None).await
    }
    pub fn close(&self) {
        let _ = self.0.close.send(true);
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
    async fn write(&self, frame: Value, response: Option<(u64, Answer)>) -> Result<(), Error> {
        self.write_gated(frame, response, &allow())
            .await
            .map_err(WriteFailure::into_error)
    }

    async fn write_gated(
        &self,
        frame: Value,
        response: Option<(u64, Answer)>,
        gate: &PreSendGate<'_>,
    ) -> Result<(), WriteFailure> {
        let before = |error: String| WriteFailure::BeforeWrite(Error::Refused(error));
        let mut bytes = serde_json::to_vec(&frame).map_err(|error| before(error.to_string()))?;
        if bytes.len() >= FRAME_LIMIT {
            self.close();
            return Err(WriteFailure::BeforeWrite(Error::Protocol(
                "outgoing frame exceeds byte limit",
            )));
        }
        bytes.push(b'\n');
        let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;
        let mut guard = tokio::time::timeout_at(deadline, self.0.writer.lock())
            .await
            .map_err(|_| before("writer wait timed out".into()))?;
        if *self.0.close.borrow() || guard.is_none() {
            return Err(WriteFailure::BeforeWrite(Error::Closed));
        }
        // The incoming owner registers before any possible write and preserves the
        // pending bound. This queue carries no frames or permission decisions.
        if let Some((id, answer)) = response {
            let (registered, receipt) = oneshot::channel();
            tokio::time::timeout_at(deadline, async {
                self.0
                    .registrations
                    .send(Registration {
                        id,
                        answer,
                        registered,
                    })
                    .await
                    .map_err(|_| Error::Closed)?;
                receipt.await.unwrap_or(Err(Error::Closed))
            })
            .await
            .map_err(|_| before("response registration timed out".into()))?
            .map_err(WriteFailure::BeforeWrite)?;
        }
        // Construct all I/O machinery before admission. The gate's final synchronous
        // check and the first write poll occur in the same task poll, with no await
        // on another owner between them.
        let mut crossing = Crossing {
            close: &self.0.close,
            armed: false,
        };
        let mut closed = self.0.close.subscribe();
        let write = async {
            if *closed.borrow() {
                return Err(WriteFailure::BeforeWrite(Error::Closed));
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
                    futures_util::future::Either::Left(_) => Err(Error::Closed),
                    futures_util::future::Either::Right((result, _)) => match result {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(_)) => Err(Error::Closed),
                        Err(_) => Err(Error::Timeout),
                    },
                }
            };
            outcome.map_err(WriteFailure::CrossedWrite)?;
            *guard = Some(writer);
            crossing.armed = false;
            Ok(())
        };
        tokio::time::timeout_at(deadline, gate.run())
            .await
            .map_err(|_| before("admission timed out".into()))?
            .map_err(|error| before(error.0))?;
        write.await
    }
}
struct Crossing<'a> {
    close: &'a watch::Sender<bool>,
    armed: bool,
}
impl Drop for Crossing<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.close.send(true);
        }
    }
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
        let (registrations, registration_rx) = mpsc::channel(64);
        let writer = Arc::new(Mutex::new(Some(Box::new(writer) as Writer)));
        let (incoming, incoming_rx) = mpsc::channel(256);
        let (close, close_rx) = watch::channel(false);
        let client = Client(Arc::new(Inner {
            registrations,
            writer: writer.clone(),
            close: close.clone(),
            next_id: AtomicU64::new(1),
        }));
        tokio::spawn(run(
            reader,
            writer,
            registration_rx,
            incoming,
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
async fn run<R>(
    reader: R,
    writer: Arc<Mutex<Option<Writer>>>,
    mut registrations: mpsc::Receiver<Registration>,
    incoming: mpsc::Sender<Incoming>,
    close: watch::Sender<bool>,
    mut closed: watch::Receiver<bool>,
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
                Ok(0) | Err(_) => Err(Error::Closed),
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
    let mut pending = HashMap::<u64, Answer>::new();
    let error = loop {
        tokio::select! {
            biased;
            _ = closed.changed() => break Error::Closed,
            registration = registrations.recv() => {
                let Some(registration) = registration else { break Error::Closed; };
                pending.retain(|_, answer| !answer.is_closed());
                if registration.registered.is_closed() { continue; }
                if pending.len() >= PENDING_LIMIT {
                    let error = Error::Protocol("too many pending requests");
                    let _ = registration.registered.send(Err(error.clone()));
                    break error;
                }
                pending.insert(registration.id, registration.answer);
                if registration.registered.send(Ok(())).is_err() {
                    pending.remove(&registration.id);
                }
            }
            frame = frame_rx.recv() => {
                let frame = match frame { Some(Ok(frame)) => frame, Some(Err(error)) => break error, None => break Error::Closed };
                if let Err(error) = receive(frame, &mut pending, &incoming) { break error; }
            }
        }
    };
    let _ = close.send(true);
    for (_, answer) in pending {
        let _ = answer.send(Err(error.clone()));
    }
    // The caller observes close while holding the writer, then releases it.
    writer.lock().await.take();
}
fn receive(
    frame: Value,
    pending: &mut HashMap<u64, Answer>,
    incoming: &mpsc::Sender<Incoming>,
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
        return incoming
            .try_send(message)
            .map_err(|_| Error::Protocol("incoming event consumer unavailable or lagged"));
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
    let _ = answer.send(response);
    Ok(())
}
fn valid_id(value: &Value) -> bool {
    value.is_string() || value.as_i64().is_some() || value.as_u64().is_some()
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod write_tests;
