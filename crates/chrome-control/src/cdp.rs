//! A minimal Chrome DevTools Protocol client over a byte stream pair, framed
//! as NUL-terminated JSON messages (`--remote-debugging-pipe`).
//!
//! - Requests carry ids; responses are matched to their caller by id.
//! - Messages without an id are events, broadcast to current subscribers and
//!   otherwise dropped.
//! - EOF, a read or write error, or [`Cdp::close`] fails every pending call
//!   with [`Error::Exited`] and refuses new ones.
//! - Every call has a timeout.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{Error, Result};

const EVENT_CAPACITY: usize = 1024;

/// A CDP event.
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub(crate) method: String,
    pub(crate) session: Option<String>,
    pub(crate) params: Value,
}

type Reply = std::result::Result<Value, String>;

struct State {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<Reply>>,
    /// `None` once closed.
    events: Option<broadcast::Sender<Event>>,
}

#[derive(Clone)]
pub(crate) struct Closer(Arc<Mutex<State>>);

impl Closer {
    /// Fails every pending call with [`Error::Exited`] and refuses new calls.
    pub(crate) fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.events = None;
        // Dropping the reply senders wakes each caller with `Exited`.
        state.pending.clear();
    }
}

pub(crate) struct Cdp {
    state: Closer,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
}

impl Cdp {
    /// Starts the reader and writer tasks on the current tokio runtime.
    pub(crate) fn start<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let state = Closer(Arc::new(Mutex::new(State {
            next_id: 1,
            pending: HashMap::new(),
            events: Some(events),
        })));
        let (outgoing, queue) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(writer, queue, state.clone()));
        tokio::spawn(read_loop(reader, state.clone()));
        Self { state, outgoing }
    }

    pub(crate) fn closer(&self) -> Closer {
        self.state.clone()
    }

    /// Events received from now on. Fails with `Exited` once closed; the
    /// receiver reports `Closed` when the client closes later.
    pub(crate) fn subscribe(&self) -> Result<broadcast::Receiver<Event>> {
        let state = self.lock();
        state
            .events
            .as_ref()
            .map(broadcast::Sender::subscribe)
            .ok_or(Error::Exited)
    }

    /// Sends one command and waits up to `timeout` for its response.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let (reply, answer) = oneshot::channel();
        let id = {
            let mut state = self.lock();
            if state.events.is_none() {
                return Err(Error::Exited);
            }
            let id = state.next_id;
            state.next_id += 1;
            state.pending.insert(id, reply);
            id
        };
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let mut bytes = message.to_string().into_bytes();
        bytes.push(0);
        if self.outgoing.send(bytes).is_err() {
            self.lock().pending.remove(&id);
            return Err(Error::Exited);
        }
        match tokio::time::timeout(timeout, answer).await {
            Err(_) => {
                self.lock().pending.remove(&id);
                Err(Error::Timeout {
                    what: format!("CDP {method}"),
                    after: timeout,
                })
            }
            Ok(Err(_)) => Err(Error::Exited),
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => Err(Error::Cdp {
                method: method.into(),
                message,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut queue: mpsc::UnboundedReceiver<Vec<u8>>,
    state: Closer,
) {
    while let Some(bytes) = queue.recv().await {
        if let Err(error) = writer.write_all(&bytes).await {
            tracing::debug!(%error, "CDP write failed");
            state.close();
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(reader: R, state: Closer) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(0, &mut buffer).await {
            Ok(0) => break,
            Ok(_) if buffer.last() != Some(&0) => break, // EOF inside a message
            Ok(_) => dispatch(&buffer[..buffer.len() - 1], &state),
            Err(error) => {
                tracing::debug!(%error, "CDP read failed");
                break;
            }
        }
    }
    state.close();
}

fn dispatch(bytes: &[u8], state: &Closer) {
    let mut message: Value = match serde_json::from_slice(bytes) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(%error, "dropping a malformed CDP message");
            return;
        }
    };
    let mut state = state.0.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let Some(reply) = state.pending.remove(&id) else {
            return; // the caller timed out
        };
        let outcome = match message.get_mut("error") {
            Some(error) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned)),
            None => Ok(message
                .get_mut("result")
                .map(Value::take)
                .unwrap_or(Value::Null)),
        };
        let _ = reply.send(outcome);
    } else if let (Some(method), Some(events)) = (
        message.get("method").and_then(Value::as_str),
        state.events.as_ref(),
    ) {
        let _ = events.send(Event {
            method: method.to_owned(),
            session: message
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            params: message
                .get_mut("params")
                .map(Value::take)
                .unwrap_or(Value::Null),
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf, duplex, split};

    const LONG: Duration = Duration::from_secs(30);

    /// The far end of an in-memory CDP connection.
    pub(crate) struct Peer {
        reader: BufReader<ReadHalf<DuplexStream>>,
        writer: WriteHalf<DuplexStream>,
    }

    impl Peer {
        pub(crate) async fn recv(&mut self) -> Option<Value> {
            let mut buffer = Vec::new();
            let read = self.reader.read_until(0, &mut buffer).await.ok()?;
            if read == 0 {
                return None;
            }
            assert_eq!(buffer.pop(), Some(0), "every message ends with NUL");
            Some(serde_json::from_slice(&buffer).expect("JSON"))
        }

        pub(crate) async fn send(&mut self, message: Value) {
            let mut bytes = message.to_string().into_bytes();
            bytes.push(0);
            self.writer.write_all(&bytes).await.unwrap();
        }
    }

    pub(crate) fn connected() -> (Cdp, Peer) {
        let (ours, theirs) = duplex(1 << 20);
        let (our_read, our_write) = split(ours);
        let (their_read, their_write) = split(theirs);
        let peer = Peer {
            reader: BufReader::new(their_read),
            writer: their_write,
        };
        (Cdp::start(our_read, our_write), peer)
    }

    #[tokio::test]
    async fn responses_are_matched_by_id_and_events_are_dispatched() {
        let (cdp, mut peer) = connected();
        let mut events = cdp.subscribe().unwrap();
        let first = cdp.call("A.one", json!({"x": 1}), None, LONG);
        let second = cdp.call("B.two", json!({}), Some("S1"), LONG);
        let server = async {
            let a = peer.recv().await.unwrap();
            let b = peer.recv().await.unwrap();
            assert_eq!(a["method"], "A.one");
            assert_eq!(a["params"]["x"], 1);
            assert!(a.get("sessionId").is_none());
            assert_eq!(b["sessionId"], "S1");
            // Answer out of order, with an event in between.
            peer.send(json!({"id": b["id"], "error": {"code": -32000, "message": "nope"}}))
                .await;
            peer.send(json!({"method": "E.happened", "sessionId": "S1", "params": {"k": 2}}))
                .await;
            peer.send(json!({"id": a["id"], "result": {"value": "a"}}))
                .await;
        };
        let (first, second, ()) = tokio::join!(first, second, server);
        assert_eq!(first.unwrap(), json!({"value": "a"}));
        match second {
            Err(Error::Cdp { method, message }) => {
                assert_eq!((method.as_str(), message.as_str()), ("B.two", "nope"));
            }
            other => panic!("expected a CDP error, got {other:?}"),
        }
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "E.happened");
        assert_eq!(event.session.as_deref(), Some("S1"));
        assert_eq!(event.params, json!({"k": 2}));
    }

    #[tokio::test]
    async fn eof_fails_every_pending_call_with_exited() {
        let (cdp, mut peer) = connected();
        let calls = async {
            tokio::join!(
                cdp.call("A.one", json!({}), None, LONG),
                cdp.call("A.two", json!({}), None, LONG),
            )
        };
        let server = async move {
            peer.recv().await.unwrap();
            peer.recv().await.unwrap();
            drop(peer); // EOF on both directions
        };
        let ((first, second), ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(calls, server)
        })
        .await
        .expect("pending calls must fail on EOF, not hang until their timeout");
        assert!(matches!(first, Err(Error::Exited)), "{first:?}");
        assert!(matches!(second, Err(Error::Exited)), "{second:?}");
        let after = cdp.call("A.three", json!({}), None, LONG).await;
        assert!(matches!(after, Err(Error::Exited)), "{after:?}");
        assert!(matches!(cdp.subscribe(), Err(Error::Exited)));
    }

    #[tokio::test]
    async fn a_call_without_a_response_times_out() {
        let (cdp, mut peer) = connected();
        let call = cdp.call("A.slow", json!({}), None, Duration::from_millis(50));
        let (result, _) = tokio::join!(call, peer.recv());
        assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
    }
}
