// Ported from L460eb6a9; exercise the current shared-client model/read APIs.
use super::*;
use std::os::fd::AsRawFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn connected() -> (
    CodexAppServer,
    NotificationStream,
    WebSocketStream<UnixStream>,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (client, peer) = tokio::join!(CodexAppServer::connect(&socket), async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    });
    let (client, notifications) = client.unwrap();
    (client, notifications, peer, root)
}

fn queued(peer: &UnixStream) -> usize {
    let mut bytes: libc::c_int = 0;
    assert_eq!(
        unsafe { libc::ioctl(peer.as_raw_fd(), libc::FIONREAD, &mut bytes) },
        0
    );
    bytes as usize
}

async fn partial_send(cancel: bool, followup: &str) {
    let (client, _notifications, mut websocket, _root) = connected().await;
    let peer = websocket.get_mut();
    let client = Arc::new(client.with_request_timeout(Duration::from_millis(100)));
    let payload = "x".repeat(8 * 1024 * 1024);
    let sending = client.clone();
    let deadline = if cancel {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(200)
    };
    let task = tokio::spawn(async move {
        tokio::time::timeout(
            deadline,
            sending.turn_start("owned", vec![InputItem::text(payload)]),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), peer.readable())
        .await
        .unwrap()
        .unwrap();
    let before_cancel = queued(peer);
    assert!(
        before_cancel > 0 && before_cancel < 1024 * 1024,
        "must cancel a genuinely incomplete WS frame"
    );
    if cancel {
        assert!(!task.is_finished());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    } else {
        assert!(
            task.await.unwrap().is_err(),
            "outer deadline must cancel the send, not a fully sent reply wait"
        );
    }
    let accepted = queued(peer);
    // No follow-up is authorized to finish the old business frame. A Ping also
    // exercises tungstenite's automatic write from its read half.
    if followup == "pong" {
        let _ = peer.write_all(&[0x89, 0]).await;
    }
    let mut received = Vec::new();
    let (drained, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(500), peer.read_to_end(&mut received)),
        async {
            if followup == "model" {
                let _ = tokio::time::timeout(
                    Duration::from_millis(200),
                    client.model_list(
                        None,
                        tokio::time::Instant::now() + Duration::from_millis(200),
                    ),
                )
                .await;
            } else if followup == "read" {
                let _ = tokio::time::timeout(
                    Duration::from_millis(200),
                    client.thread_read("owned", true),
                )
                .await;
            }
        }
    );
    assert!(
        received.len() <= accepted,
        "{} new business bytes escaped after cancellation via {followup}",
        received.len().saturating_sub(accepted)
    );
    assert!(
        drained.is_ok(),
        "cancelled incomplete transport must be shut down synchronously"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_codex_cancelled_partial_send_cannot_flush_on_model_or_read() {
    partial_send(true, "model").await;
    partial_send(true, "read").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_codex_timed_out_partial_send_cannot_flush_on_automatic_pong() {
    partial_send(false, "pong").await;
}

#[tokio::test]
async fn shared_codex_fully_sent_reply_timeout_preserves_healthy_transport() {
    let (client, _notifications, peer) = CodexAppServer::connect_pair_for_test().await;
    let client = client.with_request_timeout(Duration::from_millis(30));
    let answer = tokio::spawn(async move {
        let mut peer = peer;
        let first = peer.next().await.unwrap().unwrap();
        assert!(first.to_text().unwrap().contains("turn/start"));
        // Deliberately omit the first reply. The second request is independent.
        let second: Value =
            serde_json::from_str(peer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        peer.send(Message::Text(
            json!({"id":second["id"],"result":{"data":[],"nextCursor":null}}).to_string(),
        ))
        .await
        .unwrap();
    });
    assert!(
        client
            .turn_start("owned", vec![InputItem::text("small")])
            .await
            .is_err()
    );
    assert!(
        client
            .model_list(
                None,
                tokio::time::Instant::now() + Duration::from_millis(200)
            )
            .await
            .is_ok()
    );
    answer.await.unwrap();
}
