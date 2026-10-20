use super::*;
use crate::send_boundary::SendRefusal;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};

#[derive(Clone, Copy)]
enum Fault {
    None,
    Partial,
    Flush,
}
struct Writer {
    bytes: Arc<StdMutex<Vec<u8>>>,
    fault: Fault,
    checked: Arc<AtomicBool>,
    require_check: bool,
}
impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.require_check {
            assert!(
                self.checked.load(Ordering::SeqCst),
                "first poll must directly follow final check"
            );
        }
        let mut bytes = self.bytes.lock().unwrap();
        match self.fault {
            Fault::Partial if !bytes.is_empty() => Poll::Pending,
            Fault::Partial => {
                bytes.push(data[0]);
                Poll::Ready(Ok(1))
            }
            Fault::None | Fault::Flush => {
                bytes.extend_from_slice(data);
                Poll::Ready(Ok(data.len()))
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.fault {
            Fault::Flush => Poll::Ready(Err(std::io::Error::other("injected flush failure"))),
            Fault::None | Fault::Partial => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
fn writer(fault: Fault) -> (Mutex<Option<Writer>>, Arc<StdMutex<Vec<u8>>>) {
    let bytes = Arc::new(StdMutex::new(Vec::new()));
    (
        Mutex::new(Some(Writer {
            bytes: bytes.clone(),
            fault,
            checked: Arc::new(AtomicBool::new(false)),
            require_check: false,
        })),
        bytes,
    )
}
fn allow() -> PreSendGate<'static> {
    PreSendGate::new(|| Box::pin(async { Ok(()) }))
}

#[tokio::test]
async fn claude_line_gate_runs_under_writer_lock_after_wait() {
    let (stdin, bytes) = writer(Fault::None);
    let allowed = AtomicBool::new(true);
    let gate = PreSendGate::new(|| {
        Box::pin(async {
            assert!(stdin.try_lock().is_err());
            if allowed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(SendRefusal("revoked".into()))
            }
        })
    });
    let held = stdin.lock().await;
    let mut write = Box::pin(write_line(&stdin, "user", &gate));
    assert!(futures_util::poll!(&mut write).is_pending());
    allowed.store(false, Ordering::SeqCst);
    drop(held);
    assert!(
        matches!(write.await, Err(LineWriteError::BeforeWrite(message)) if message == "revoked")
    );
    assert!(bytes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn claude_line_final_check_has_no_yield_before_first_poll() {
    let (stdin, bytes) = writer(Fault::None);
    let checked = stdin.lock().await.as_ref().unwrap().checked.clone();
    stdin.lock().await.as_mut().unwrap().require_check = true;
    let gate = PreSendGate::new(|| {
        Box::pin(async {
            // Reset on the next scheduling turn: a yield after checking is observable.
            let reset = checked.clone();
            tokio::spawn(async move {
                reset.store(false, Ordering::SeqCst);
            });
            checked.store(true, Ordering::SeqCst);
            Ok(())
        })
    });
    write_line(&stdin, "user", &gate).await.unwrap();
    assert_eq!(*bytes.lock().unwrap(), b"user\n");
}

#[tokio::test(start_paused = true)]
async fn claude_line_closed_or_gate_timeout_is_before_write() {
    let (stdin, bytes) = writer(Fault::None);
    stdin.lock().await.take();
    assert!(
        matches!(write_line(&stdin, "user", &allow()).await, Err(LineWriteError::BeforeWrite(message)) if message == "claude stdin is already closed")
    );
    let (stdin, gate_bytes) = writer(Fault::None);
    let gate = PreSendGate::new(|| Box::pin(std::future::pending()));
    let start = tokio::time::Instant::now();
    assert!(matches!(
        write_line(&stdin, "user", &gate).await,
        Err(LineWriteError::BeforeWrite(_))
    ));
    assert_eq!(start.elapsed(), ADMISSION_TIMEOUT);
    assert!(bytes.lock().unwrap().is_empty());
    assert!(gate_bytes.lock().unwrap().is_empty());
    let held = stdin.lock().await;
    assert!(matches!(
        write_line(&stdin, "user", &allow()).await,
        Err(LineWriteError::BeforeWrite(_))
    ));
    drop(held);
    assert!(gate_bytes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn claude_line_write_timeout_and_flush_failure_are_crossed() {
    for fault in [Fault::Partial, Fault::Flush] {
        let (stdin, bytes) = writer(fault);
        let error = write_line(&stdin, "user", &allow()).await.unwrap_err();
        match fault {
            Fault::Partial => {
                assert!(
                    matches!(error, LineWriteError::CrossedWrite(ref message) if message.to_string() == "claude did not take its stdin line in time")
                );
                assert_eq!(*bytes.lock().unwrap(), b"u");
            }
            Fault::Flush => {
                assert!(
                    matches!(error, LineWriteError::CrossedWrite(ref message) if message.to_string() == "injected flush failure")
                );
                assert_eq!(*bytes.lock().unwrap(), b"user\n");
            }
            Fault::None => panic!("fault required"),
        }
        assert!(
            stdin.lock().await.is_none(),
            "uncertain writes close stdin under the writer lock"
        );
        assert!(matches!(
            write_line(&stdin, "next", &allow()).await,
            Err(LineWriteError::BeforeWrite(_))
        ));
        match fault {
            Fault::Partial => assert_eq!(*bytes.lock().unwrap(), b"u"),
            Fault::Flush => assert_eq!(*bytes.lock().unwrap(), b"user\n"),
            Fault::None => panic!("fault required"),
        }
    }
}

#[tokio::test]
async fn claude_line_cancelled_partial_write_closes_before_unlock() {
    let (stdin, bytes) = writer(Fault::Partial);
    let gate = allow();
    let mut write = Box::pin(write_line(&stdin, "user", &gate));
    assert!(futures_util::poll!(&mut write).is_pending());
    assert_eq!(*bytes.lock().unwrap(), b"u");
    assert!(stdin.try_lock().is_err());
    drop(write);
    assert!(stdin.lock().await.is_none());
    assert!(matches!(
        write_line(&stdin, "next", &gate).await,
        Err(LineWriteError::BeforeWrite(_))
    ));
    assert_eq!(*bytes.lock().unwrap(), b"u");
}

#[tokio::test(start_paused = true)]
async fn claude_line_cancelled_admission_writes_nothing_and_write_budget_is_separate() {
    let (stdin, bytes) = writer(Fault::Partial);
    let pending = PreSendGate::new(|| Box::pin(std::future::pending()));
    let mut write = Box::pin(write_line(&stdin, "user", &pending));
    assert!(futures_util::poll!(&mut write).is_pending());
    drop(write);
    assert!(stdin.lock().await.is_none());
    assert!(bytes.lock().unwrap().is_empty());
    let (stdin, bytes) = writer(Fault::Partial);
    let gate = PreSendGate::new(|| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(900)).await;
            Ok(())
        })
    });
    let started = tokio::time::Instant::now();
    assert!(matches!(
        write_line(&stdin, "user", &gate).await,
        Err(LineWriteError::CrossedWrite(LineIoError::Timeout))
    ));
    assert_eq!(
        started.elapsed(),
        Duration::from_millis(900) + WRITE_TIMEOUT
    );
    assert_eq!(*bytes.lock().unwrap(), b"u");
}
