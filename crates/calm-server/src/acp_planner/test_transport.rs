//! Private test fault injection at the real ACP transport's prompt writer.
#![cfg(test)]
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::Notify,
};
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    Partial,
    Flush,
    Timeout,
}
struct Hook {
    fault: Fault,
    bytes: Arc<Mutex<Vec<u8>>>,
    crossed: Arc<Notify>,
}
static HOOKS: LazyLock<Mutex<HashMap<String, Hook>>> = LazyLock::new(Mutex::default);
pub(crate) struct Probe {
    pub(crate) bytes: Arc<Mutex<Vec<u8>>>,
    pub(crate) crossed: Arc<Notify>,
}
pub(crate) fn arm(worker: &str, fault: Fault) -> Probe {
    let bytes = Arc::default();
    let crossed = Arc::default();
    HOOKS.lock().unwrap().insert(
        worker.into(),
        Hook {
            fault,
            bytes: Arc::clone(&bytes),
            crossed: Arc::clone(&crossed),
        },
    );
    Probe { bytes, crossed }
}
pub(crate) fn connection(worker: &str) -> Option<provider::acp::Connection> {
    let hook = HOOKS.lock().unwrap().remove(worker)?;
    Some(provider::acp::Connection::new(
        PendingRead,
        FaultWrite { hook, wrote: false },
    ))
}
struct PendingRead;
impl AsyncRead for PendingRead {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}
struct FaultWrite {
    hook: Hook,
    wrote: bool,
}
impl AsyncWrite for FaultWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if !self.wrote {
            let count = match self.hook.fault {
                Fault::Flush => bytes.len(),
                Fault::Partial | Fault::Timeout => 7.min(bytes.len()),
            };
            self.hook
                .bytes
                .lock()
                .unwrap()
                .extend_from_slice(&bytes[..count]);
            self.wrote = true;
            self.hook.crossed.notify_one();
            return Poll::Ready(Ok(count));
        }
        match self.hook.fault {
            Fault::Partial => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
            Fault::Timeout => Poll::Pending,
            Fault::Flush => Poll::Ready(Ok(bytes.len())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.hook.fault {
            Fault::Flush => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
            Fault::Partial | Fault::Timeout => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
