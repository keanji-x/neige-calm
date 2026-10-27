use super::{Error, Pending, Result};
use std::os::fd::AsFd;
use std::sync::Mutex;

pub(super) fn websocket_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    // thread/resume emits the entire retained history as ONE frame (#3039).
    // A frame may reach the existing 64 MiB message bound, never exceed it.
    config.max_frame_size = config.max_message_size;
    config
}

/// A separately OWNED descriptor for the same socket, never a saved raw fd.
/// shutdown reaches both split WS halves immediately, including read-side Pong
/// flushing. Aborting a reader task alone only schedules its later destruction.
pub(super) struct TransportAbort {
    socket: std::os::unix::net::UnixStream,
    failure: Mutex<Option<String>>,
}
impl TransportAbort {
    pub(super) fn new(stream: &tokio::net::UnixStream) -> std::io::Result<Self> {
        Ok(Self {
            socket: stream.as_fd().try_clone_to_owned()?.into(),
            failure: Mutex::new(None),
        })
    }
    pub(super) fn check(&self) -> Result<()> {
        if let Some(reason) = self.failure.lock().unwrap().as_ref() {
            return Err(Error::Transport(reason.clone()));
        }
        Ok(())
    }
    pub(super) fn poison(&self) {
        self.close("transport aborted after incomplete write; outcome unknown".into());
    }
    pub(super) fn close(&self, reason: String) {
        self.failure.lock().unwrap().get_or_insert(reason);
        // No WebSocket close/flush: buffered application bytes must be discarded.
        loop {
            match self.socket.shutdown(std::net::Shutdown::Both) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }
    pub(super) fn sending(&self) -> Result<IncompleteSend<'_>> {
        self.check()?;
        Ok(IncompleteSend {
            transport: self,
            complete: false,
        })
    }
}

pub(super) struct IncompleteSend<'a> {
    transport: &'a TransportAbort,
    complete: bool,
}
impl IncompleteSend<'_> {
    pub(super) fn complete(&mut self) {
        self.complete = true;
    }
}
impl Drop for IncompleteSend<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.transport.poison();
        }
    }
}

/// A cancelled response wait removes only its correlation entry. A fully flushed
/// request can time out without poisoning a healthy shared connection.
pub(super) struct PendingRequest {
    pending: Pending,
    id: u64,
}
impl PendingRequest {
    pub(super) fn new(pending: Pending, id: u64) -> Self {
        Self { pending, id }
    }
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

/// Constructed before spawning the reader, so even an unpolled task abort records
/// its cause before waking pending RPCs or notification consumers.
pub(super) struct ReaderLifetime {
    pub(super) sender: tokio::sync::mpsc::UnboundedSender<super::Notification>,
    pub(super) pending: super::Pending,
    transport: std::sync::Arc<TransportAbort>,
}
impl ReaderLifetime {
    pub(super) fn new(
        sender: tokio::sync::mpsc::UnboundedSender<super::Notification>,
        pending: super::Pending,
        transport: std::sync::Arc<TransportAbort>,
    ) -> Self {
        Self {
            sender,
            pending,
            transport,
        }
    }
}
impl Drop for ReaderLifetime {
    fn drop(&mut self) {
        self.transport
            .close("WebSocket reader or dispatch owner dropped; pending outcomes unknown".into());
        // Retire RPC waiters before dropping the notification sender. This guard
        // also runs on panic/cancellation, including before the task's first poll.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}
