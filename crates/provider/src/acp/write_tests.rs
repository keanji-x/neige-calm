use super::*;
use crate::send_boundary::{PreSendGate, SendRefusal};
use serde_json::json;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, BufReader};

fn allow() -> PreSendGate<'static> {
    PreSendGate::new(|| Box::pin(async { Ok(()) }))
}
fn gate(live: Arc<AtomicBool>) -> PreSendGate<'static> {
    PreSendGate::new(move || {
        let live = live.clone();
        Box::pin(async move {
            if live.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(SendRefusal("revoked".into()))
            }
        })
    })
}
#[tokio::test]
async fn writer_wait_rechecks_authority_and_refusal_writes_zero_bytes() {
    let (output, mut peer) = tokio::io::duplex(4096);
    let (input, _input_peer) = tokio::io::duplex(4096);
    let connection = Connection::new(input, output);
    let lock = connection.client.0.writer.lock().await;
    let live = Arc::new(AtomicBool::new(true));
    let gate = gate(live.clone());
    let send = connection
        .client
        .submit_gated("session/new", json!({}), &gate);
    tokio::pin!(send);
    assert!(futures_util::poll!(&mut send).is_pending());
    live.store(false, Ordering::SeqCst);
    drop(lock);
    assert!(matches!(send.await, Err(WriteFailure::BeforeWrite(_))));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.read_u8())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancelled_before_crossing_keeps_writer_usable() {
    let (input, _input_peer) = tokio::io::duplex(4096);
    let (output, mut peer) = tokio::io::duplex(4096);
    let connection = Connection::new(input, output);
    let entered = tokio::sync::Notify::new();
    let gate = PreSendGate::new(|| {
        Box::pin(async {
            entered.notify_one();
            std::future::pending().await
        })
    });
    let mut send = Box::pin(
        connection
            .client
            .submit_gated("session/new", json!({}), &gate),
    );
    tokio::select! { _=&mut send=>panic!("admission must wait"), _=entered.notified()=>{} }
    drop(send);
    let pending = connection
        .client
        .submit_gated("ok", json!({}), &allow())
        .await
        .unwrap();
    let mut frame = String::new();
    BufReader::new(&mut peer)
        .read_line(&mut frame)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&frame).unwrap()["method"],
        "ok"
    );
    drop(pending);
}
#[tokio::test]
async fn cancellation_after_crossing_fences_partial_frame() {
    let (input, _input_peer) = tokio::io::duplex(4096);
    let (output, mut peer) = tokio::io::duplex(1);
    let connection = Connection::new(input, output);
    let gate = allow();
    let mut send = Box::pin(
        connection
            .client
            .submit_gated("session/prompt", json!({}), &gate),
    );
    assert!(futures_util::poll!(&mut send).is_pending());
    // Registration is asynchronous; wait until the first byte actually crosses.
    tokio::select! { _=&mut send=>panic!("write must block"), byte=peer.read_u8()=>assert_eq!(byte.unwrap(),b'{') }
    drop(send);
    assert!(matches!(
        connection
            .client
            .submit_gated("again", json!({}), &gate)
            .await,
        Err(WriteFailure::BeforeWrite(_))
    ));
}
struct FlushFailure;
impl AsyncWrite for FlushFailure {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("flush")))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn flush_failure_is_crossed_and_fences_connection() {
    let (input, _peer) = tokio::io::duplex(4096);
    let connection = Connection::new(input, FlushFailure);
    assert!(matches!(
        connection
            .client
            .submit_gated("session/new", json!({}), &allow())
            .await,
        Err(WriteFailure::CrossedWrite(Error::Closed))
    ));
    assert!(matches!(
        connection
            .client
            .submit_gated("again", json!({}), &allow())
            .await,
        Err(WriteFailure::BeforeWrite(_))
    ));
}
#[tokio::test(start_paused = true)]
async fn admission_timeout_is_before_write_and_write_timeout_is_crossed() {
    let (input, _input_peer) = tokio::io::duplex(4096);
    let (output, _peer) = tokio::io::duplex(1);
    let connection = Connection::new(input, output);
    let gate = PreSendGate::new(|| Box::pin(std::future::pending()));
    let start = tokio::time::Instant::now();
    assert!(matches!(
        connection
            .client
            .submit_gated("session/new", json!({}), &gate)
            .await,
        Err(WriteFailure::BeforeWrite(_))
    ));
    assert_eq!(start.elapsed(), ADMISSION_TIMEOUT);
    assert!(matches!(
        connection
            .client
            .submit_gated("session/new", json!({}), &allow())
            .await,
        Err(WriteFailure::CrossedWrite(Error::Timeout))
    ));
    assert_eq!(start.elapsed(), ADMISSION_TIMEOUT + WRITE_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn writer_wait_timeout_is_before_write_and_does_not_poison_connection() {
    let (input, _input_peer) = tokio::io::duplex(4096);
    let (output, mut peer) = tokio::io::duplex(4096);
    let connection = Connection::new(input, output);
    let lock = connection.client.0.writer.lock().await;
    assert!(matches!(
        connection
            .client
            .submit_gated("blocked", json!({}), &allow())
            .await,
        Err(WriteFailure::BeforeWrite(_))
    ));
    drop(lock);
    let pending = connection
        .client
        .submit_gated("ok", json!({}), &allow())
        .await
        .unwrap();
    let mut frame = String::new();
    BufReader::new(&mut peer)
        .read_line(&mut frame)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&frame).unwrap()["method"],
        "ok"
    );
    drop(pending);
}

#[tokio::test]
async fn pending_bound_is_preserved_and_abandoned_admissions_do_not_consume_it() {
    let (input, _input_peer) = tokio::io::duplex(4096);
    let (output, mut peer) = tokio::io::duplex(65536);
    let connection = Connection::new(input, output);
    let refuse = PreSendGate::new(|| Box::pin(async { Err(SendRefusal("no".into())) }));
    for _ in 0..PENDING_LIMIT + 1 {
        assert!(matches!(
            connection
                .client
                .submit_gated("refused", json!({}), &refuse)
                .await,
            Err(WriteFailure::BeforeWrite(Error::Refused(_)))
        ));
    }
    let mut pending = Vec::new();
    for _ in 0..PENDING_LIMIT {
        pending.push(
            connection
                .client
                .submit_gated("ok", json!({}), &allow())
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        connection
            .client
            .submit_gated("overflow", json!({}), &allow())
            .await,
        Err(WriteFailure::BeforeWrite(Error::Protocol(
            "too many pending requests"
        )))
    ));
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(
        bytes.iter().filter(|byte| **byte == b'\n').count(),
        PENDING_LIMIT
    );
    for response in pending {
        assert_eq!(
            response.wait(Duration::from_secs(1)).await,
            Err(Error::Protocol("too many pending requests"))
        );
    }
}

struct CheckFirstPoll(Arc<AtomicBool>);
impl AsyncWrite for CheckFirstPoll {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        assert!(
            self.0.load(Ordering::SeqCst),
            "yielded after final check, before first write poll"
        );
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn final_check_has_no_cooperative_yield_before_first_write_poll() {
    use std::future::Future;
    let checked = Arc::new(AtomicBool::new(false));
    let (input, _peer) = tokio::io::duplex(4096);
    let connection = Connection::new(input, CheckFirstPoll(checked.clone()));
    let gate = PreSendGate::new(|| {
        let checked = checked.clone();
        Box::pin(std::future::poll_fn(move |cx| {
            // Exhaust the task's cooperative budget before the final synchronous
            // check. No scheduler-aware wrapper may yield between it and poll_write.
            loop {
                let consume = tokio::task::consume_budget();
                tokio::pin!(consume);
                if consume.as_mut().poll(cx).is_pending() {
                    break;
                }
            }
            let reset = checked.clone();
            tokio::spawn(async move {
                reset.store(false, Ordering::SeqCst);
            });
            checked.store(true, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }))
    });
    let pending = connection
        .client
        .submit_gated("session/prompt", json!({}), &gate)
        .await
        .unwrap();
    drop(pending);
}
