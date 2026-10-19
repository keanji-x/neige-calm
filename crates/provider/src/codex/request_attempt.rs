//! Per-RPC transport evidence. These facts neither authorize retry nor prove
//! prompt acceptance. In particular, a remote error is still after crossing.
use super::client_transport::PendingRequest;
use super::{CodexAppServer, Error, Message, Value};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::json;
use std::fmt;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestCrossing {
    /// This call returned before entering the socket send.
    BeforeSend,
    /// Sending was entered; an error does not establish whether input arrived.
    AfterSend,
}

/// A provider-reported JSON-RPC error object (`-32600` etc.), retained without a
/// serialization implementation. Debug omits message/data, which may contain request content.
#[derive(Clone, Deserialize)]
pub(super) struct RpcRemoteError {
    pub(super) code: i64,
    pub(super) message: String,
    #[serde(default)]
    pub(super) data: Option<Value>,
}

impl fmt::Debug for RpcRemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcRemoteError")
            .field("code", &self.code)
            .field("has_data", &self.data.is_some())
            .finish_non_exhaustive()
    }
}

/// What the reader routed to a pending request instead of a result.
pub(super) enum RpcResponseError {
    Remote(RpcRemoteError),
    Malformed(Value),
}

pub(super) enum FailureCause {
    Local(Error),
    Response(RpcResponseError),
}

/// Only the request owner constructs crossing evidence. A cancelled future
/// produces no value: its caller must keep any durable dispatch intent unknown.
pub(super) struct RpcAttemptFailure {
    pub(super) method: String,
    /// Connection-local JSON-RPC correlation, never a durable submission ID.
    pub(super) request_id: Option<u64>,
    pub(super) crossing: RequestCrossing,
    pub(super) cause: FailureCause,
}

impl RpcAttemptFailure {
    fn local(method: &str, id: Option<u64>, crossing: RequestCrossing, error: Error) -> Self {
        Self {
            method: method.into(),
            request_id: id,
            crossing,
            cause: FailureCause::Local(error),
        }
    }

    fn response(method: &str, id: u64, error: RpcResponseError) -> Self {
        Self {
            method: method.into(),
            request_id: Some(id),
            crossing: RequestCrossing::AfterSend,
            cause: FailureCause::Response(error),
        }
    }

    /// Compatibility for callers whose contract is only an RPC result. A JSON-RPC
    /// `error` frame maps to [`Error::Refused`]; everything else keeps its local error.
    pub(super) fn into_legacy_error(self) -> Error {
        match self.cause {
            FailureCause::Local(error) => error,
            // The one place codex's own refusal is still distinguishable from everything else that can go wrong.
            FailureCause::Response(RpcResponseError::Remote(error)) => Error::Refused(format!(
                "{} failed: {} (code {})",
                self.method, error.message, error.code
            )),
            FailureCause::Response(RpcResponseError::Malformed(raw)) => Error::Refused(format!(
                "{} failed: malformed error frame: {raw} (code 0)",
                self.method
            )),
        }
    }
}

impl fmt::Debug for RpcAttemptFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcAttemptFailure")
            .field("method", &self.method)
            .field("request_id", &self.request_id)
            .field("crossing", &self.crossing)
            .finish_non_exhaustive()
    }
}

impl CodexAppServer {
    /// [`Self::request_attempt_until`] with the client's request timeout.
    pub(super) async fn request_attempt<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<T, RpcAttemptFailure> {
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        self.request_attempt_until(method, params, deadline).await
    }

    /// The one request implementation behind result-only and evidence-aware APIs.
    /// Entering `send` is the conservative crossing; neither timeout nor socket
    /// abort afterwards can produce a BeforeSend result for this request.
    /// Cancellation removes the pending entry; if it interrupts an incomplete send
    /// the owned socket is shut down unflushed and the outcome stays unknown.
    pub(super) async fn request_attempt_until<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
    ) -> std::result::Result<T, RpcAttemptFailure> {
        let before =
            |id, error| RpcAttemptFailure::local(method, id, RequestCrossing::BeforeSend, error);
        // An already-spent budget is answered without touching the wire: writing the frame would put a request on the daemon whose answer we already decided to ignore.
        if tokio::time::Instant::now() >= deadline {
            return Err(before(
                None,
                Error::Transport(format!(
                    "request {method} skipped: the caller's budget was already spent"
                )),
            ));
        }
        self.transport
            .check()
            .map_err(|error| before(None, error))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Every return and cancellation below drops the now-unanswerable pending entry.
        let _pending = PendingRequest::new(self.pending.clone(), id);
        let text = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|error| before(Some(id), Error::Serde(error)))?;
        let after =
            |error| RpcAttemptFailure::local(method, Some(id), RequestCrossing::AfterSend, error);

        // Write under the sink lock so concurrent requests don't interleave WS frames.
        {
            let mut sink = self.sink.lock().await;
            // Created after the lock: cancellation closes the owned socket BEFORE releasing the sink.
            let mut sending = self
                .transport
                .sending()
                .map_err(|error| before(Some(id), error))?;
            sink.send(Message::Text(text))
                .await
                .map_err(|error| after(Error::Transport(format!("send {method}: {error}"))))?;
            sending.complete();
        }

        tracing::trace!(id, method, "codex app-server: request sent");

        // This timer is not a turn lifecycle criterion.
        let outcome = tokio::time::timeout_at(deadline, rx)
            .await
            .map_err(|_| after(Error::Transport(format!("request {method} timed out"))))?;
        match outcome {
            Ok(Ok(value)) => serde_json::from_value(value).map_err(|error| {
                after(Error::Transport(format!("decode {method} result: {error}")))
            }),
            Ok(Err(error)) => Err(RpcAttemptFailure::response(method, id, error)),
            // Sender dropped without sending: the reader task ended before our response arrived.
            Err(_) => Err(after(Error::Transport(format!(
                "{method}: connection closed before response"
            )))),
        }
    }
}
