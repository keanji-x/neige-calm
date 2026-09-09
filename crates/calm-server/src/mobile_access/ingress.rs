use super::pairing::PairingState;
use axum::serve::Listener;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// Cancellation reaches the actual upgraded transport, including WebSockets.
/// Merely removing a session or gracefully stopping the listener is insufficient.
pub(super) struct RevocableListener {
    pub listener: TcpListener,
    pub state: Arc<Mutex<PairingState>>,
}

pub(super) struct RevocableIo {
    stream: TcpStream,
    read_revoked: Pin<Box<dyn Future<Output = ()> + Send>>,
    write_revoked: Pin<Box<dyn Future<Output = ()> + Send>>,
    closed: bool,
}

impl Listener for RevocableListener {
    type Io = RevocableIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, addr)) => {
                    let Ok(state) = self.state.lock() else {
                        continue;
                    };
                    let read_revoked = Box::pin(state.connections.clone().cancelled_owned());
                    let write_revoked = Box::pin(state.connections.clone().cancelled_owned());
                    return (
                        RevocableIo {
                            stream,
                            read_revoked,
                            write_revoked,
                            closed: false,
                        },
                        addr,
                    );
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl RevocableIo {
    fn check(&mut self, cx: &mut Context<'_>, writing: bool) -> io::Result<()> {
        let revoked = if writing {
            &mut self.write_revoked
        } else {
            &mut self.read_revoked
        };
        if self.closed || revoked.as_mut().poll(cx).is_ready() {
            self.closed = true;
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Mobile connection revoked",
            ))
        } else {
            Ok(())
        }
    }
}

impl AsyncRead for RevocableIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Err(error) = self.check(cx, false) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for RevocableIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Err(error) = self.check(cx, true) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Err(error) = self.check(cx, true) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Wake, Waker};

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);
    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn mobile_access_revocation_wakes_reader_after_a_separate_writer_poll() {
        let state = Arc::new(Mutex::new(PairingState::default()));
        let mut listener = RevocableListener {
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            state: state.clone(),
        };
        let _peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut io, _) = listener.accept().await;
        let reader = Arc::new(WakeCounter::default());
        let writer = Arc::new(WakeCounter::default());
        let reader_waker = Waker::from(reader.clone());
        let writer_waker = Waker::from(writer.clone());
        let mut bytes = [0u8; 1];
        assert!(
            Pin::new(&mut io)
                .poll_read(
                    &mut Context::from_waker(&reader_waker),
                    &mut ReadBuf::new(&mut bytes)
                )
                .is_pending()
        );
        let _ = Pin::new(&mut io).poll_write(&mut Context::from_waker(&writer_waker), b"hello");
        let before = reader.0.load(Ordering::SeqCst);
        state.lock().unwrap().connections.cancel();
        assert!(
            reader.0.load(Ordering::SeqCst) > before,
            "a writer must not replace a blocked reader's cancellation wakeup"
        );
    }
}
