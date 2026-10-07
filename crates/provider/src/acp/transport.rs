use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

const FRAME_LIMIT: usize = 8 * 1024 * 1024;
const PENDING_LIMIT: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("ACP connection closed; dispatched request outcomes may be unknown")]
    Closed,
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
struct Command {
    frame: Value,
    response: Option<(u64, Answer)>,
    written: oneshot::Sender<Result<(), Error>>,
}
struct Inner {
    commands: mpsc::Sender<Command>,
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
    pub async fn submit(&self, method: &str, params: Value) -> Result<PendingResponse, Error> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, response) = oneshot::channel();
        self.write(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            Some((id, answer)),
        )
        .await?;
        Ok(PendingResponse {
            response,
            close: self.0.close.clone(),
            settled: false,
        })
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        self.submit(method, params).await?.wait(timeout).await
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.write(
            json!({"jsonrpc":"2.0","method":method,"params":params}),
            None,
        )
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
    async fn write(&self, frame: Value, response: Option<(u64, Answer)>) -> Result<(), Error> {
        if *self.0.close.borrow() {
            return Err(Error::Closed);
        }
        let (written, receipt) = oneshot::channel();
        let dispatch = async {
            self.0
                .commands
                .send(Command {
                    frame,
                    response,
                    written,
                })
                .await
                .map_err(|_| Error::Closed)?;
            receipt.await.unwrap_or(Err(Error::Closed))
        };
        match tokio::time::timeout(WRITE_TIMEOUT, dispatch).await {
            Ok(outcome) => outcome,
            Err(_) => {
                self.close();
                Err(Error::Timeout)
            }
        }
    }
}
impl Connection {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (commands, command_rx) = mpsc::channel(64);
        let (incoming, incoming_rx) = mpsc::channel(256);
        let (close, close_rx) = watch::channel(false);
        let client = Client(Arc::new(Inner {
            commands,
            close: close.clone(),
            next_id: AtomicU64::new(1),
        }));
        tokio::spawn(run(reader, writer, command_rx, incoming, close, close_rx));
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
async fn run<R, W>(
    reader: R,
    mut writer: W,
    mut commands: mpsc::Receiver<Command>,
    incoming: mpsc::Sender<Incoming>,
    close: watch::Sender<bool>,
    mut closed: watch::Receiver<bool>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
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
            command = commands.recv() => {
                let Some(command) = command else { break Error::Closed; };
                if command.written.is_closed() { continue; }
                if pending.len() >= PENDING_LIMIT && command.response.is_some() {
                    let _ = command.written.send(Err(Error::Protocol("too many pending requests")));
                    break Error::Protocol("too many pending requests");
                }
                let mut bytes = match serde_json::to_vec(&command.frame) {
                    Ok(bytes) if bytes.len() < FRAME_LIMIT => bytes,
                    _ => { let _ = command.written.send(Err(Error::Protocol("outgoing frame exceeds byte limit"))); break Error::Protocol("outgoing frame exceeds byte limit"); }
                };
                bytes.push(b'\n');
                if let Some((id, answer)) = command.response { pending.insert(id, answer); }
                let write = async { writer.write_all(&bytes).await?; writer.flush().await };
                let outcome = tokio::select! {
                    _ = closed.changed() => Err(Error::Closed),
                    result = tokio::time::timeout(WRITE_TIMEOUT, write) => match result { Ok(Ok(())) => Ok(()), Ok(Err(_)) => Err(Error::Closed), Err(_) => Err(Error::Timeout) },
                };
                if command.written.send(outcome.clone()).is_err() { break Error::Closed; }
                if let Err(error) = outcome { break error; }
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
    // Drop the pipe rather than appending to a potentially partial frame.
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
