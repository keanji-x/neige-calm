//! Real transport regression coverage for terminal cause propagation.
use super::*;
#[tokio::test]
async fn disconnect_read_error_preserves_io_kind_without_payload() {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };
    struct BrokenRead(tokio::sync::oneshot::Receiver<()>);
    impl tokio::io::AsyncRead for BrokenRead {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if Pin::new(&mut self.0).poll(cx).is_pending() {
                return Poll::Pending;
            }
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "secret frame contents",
            )))
        }
    }
    let (fail, failed) = tokio::sync::oneshot::channel();
    let connection = Connection::new(BrokenRead(failed), tokio::io::sink());
    let pending = connection
        .client
        .submit_gated("session/prompt", json!({}), &allow())
        .await
        .unwrap();
    fail.send(()).unwrap();
    let error = pending.wait(BUDGET).await.unwrap_err();
    assert!(
        error.to_string().contains("InvalidData"),
        "read failure must retain its category: {error}"
    );
    assert!(!error.to_string().contains("secret frame"));
    connection.client.close();
    assert_eq!(
        connection.client.terminal_error(),
        Some(error),
        "cleanup cannot replace the first error"
    );
}

#[tokio::test]
async fn disconnect_protocol_error_is_published_before_incoming_eof_and_survives_cleanup() {
    let (mut connection, mut reader, mut writer) = peer();
    let pending = connection
        .client
        .submit_gated("session/prompt", json!({}), &allow())
        .await
        .unwrap();
    read(&mut reader).await;
    writer
        .write_all(b"not-json secret=do-not-log\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(BUDGET, connection.incoming.recv())
            .await
            .unwrap()
            .is_none()
    );
    let error = Error::Protocol("invalid JSON frame");
    assert_eq!(connection.client.terminal_error(), Some(error.clone()));
    connection.client.close();
    assert_eq!(pending.wait(BUDGET).await, Err(error.clone()));
    assert_eq!(connection.client.terminal_error(), Some(error));
}

#[tokio::test]
async fn disconnect_after_response_keeps_the_rpc_result() {
    let (mut connection, mut reader, mut writer) = peer();
    let pending = connection
        .client
        .submit_gated("session/prompt", json!({}), &allow())
        .await
        .unwrap();
    let request = read(&mut reader).await;
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"stopReason":"end_turn"}}),
    )
    .await;
    writer.shutdown().await.unwrap();
    drop(writer);
    assert!(
        tokio::time::timeout(BUDGET, connection.incoming.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        pending.wait(BUDGET).await.unwrap(),
        json!({"stopReason":"end_turn"})
    );
}

#[tokio::test]
async fn disconnect_full_notification_queue_fails_prompt_without_silently_dropping_events() {
    let (connection, mut reader, mut writer) = peer();
    let pending = connection
        .client
        .submit_gated("session/prompt", json!({}), &allow())
        .await
        .unwrap();
    read(&mut reader).await;
    let frame = json!({"jsonrpc":"2.0","method":"session/update","params":{}}).to_string() + "\n";
    writer
        .write_all(frame.repeat(257).as_bytes())
        .await
        .unwrap();
    let error = Error::Protocol("incoming event queue full");
    assert_eq!(pending.wait(BUDGET).await, Err(error.clone()));
    assert_eq!(connection.client.terminal_error(), Some(error));
    assert_eq!(connection.incoming.len(), 256);
}
