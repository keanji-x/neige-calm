use super::{CalmError, Pending, Result};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// A separately OWNED descriptor for the same socket, never a saved raw fd.
/// shutdown reaches both split WS halves immediately, including read-side Pong
/// flushing. Aborting a reader task alone only schedules its later destruction.
pub(super) struct TransportAbort {
    socket: std::os::unix::net::UnixStream,
    poisoned: AtomicBool,
}
impl TransportAbort {
    pub(super) fn new(stream: &tokio::net::UnixStream) -> std::io::Result<Self> {
        Ok(Self {
            socket: stream.as_fd().try_clone_to_owned()?.into(),
            poisoned: AtomicBool::new(false),
        })
    }
    pub(super) fn check(&self) -> Result<()> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(CalmError::CodexAppServer(
                "transport aborted after incomplete write; outcome unknown".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
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
