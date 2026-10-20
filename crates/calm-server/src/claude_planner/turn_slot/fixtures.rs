//! Fault only the real writer's flush, after write_all has reached the child pipe.
use super::*;
use futures_util::future::BoxFuture;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;

pub(crate) struct LineStdin {
    child: ChildStdin,
    pub(crate) flush_failure: Option<BoxFuture<'static, std::io::Result<()>>>,
}
impl From<ChildStdin> for LineStdin {
    fn from(child: ChildStdin) -> Self {
        Self {
            child,
            flush_failure: None,
        }
    }
}
impl AsyncWrite for LineStdin {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.child).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.flush_failure {
            Some(failure) => failure.as_mut().poll(cx),
            None => Pin::new(&mut self.child).poll_flush(cx),
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.child).poll_shutdown(cx)
    }
}
