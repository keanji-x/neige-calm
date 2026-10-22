//! Regression for #3039: app-server emits complete history in a single frame.
use super::*;
use futures_util::SinkExt;

async fn connection() -> (
    CodexAppServer,
    NotificationStream,
    WebSocketStream<UnixStream>,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let (client, peer) = tokio::join!(CodexAppServer::connect(&path), async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    });
    let (client, notifications) = client.unwrap();
    (client, notifications, peer, root)
}

#[tokio::test]
async fn resume_history_above_default_frame_limit_keeps_connection_usable() {
    let (client, _notifications, mut peer, _root) = connection().await;
    let serving = async {
        let request: Value =
            serde_json::from_str(peer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "thread/resume");
        // The incident's real 0.163.0 response was 17,348,935 bytes, in one frame.
        let response = json!({"id":request["id"],"result":{"thread":{"id":"large","history":"x".repeat(17_348_935)}}});
        let _ = peer.send(Message::Text(response.to_string())).await;
        if let Some(Ok(Message::Text(raw))) = peer.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(request["method"], "model/list");
            peer.send(Message::Text(
                json!({"id":request["id"],"result":{"data":[],"nextCursor":null}}).to_string(),
            ))
            .await
            .unwrap();
        }
    };
    let requesting = async {
        let result = client.thread_resume("large").await;
        assert!(result.is_ok(), "valid resume response must fit: {result:?}");
        client
            .model_list(None, tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(serving, requesting);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn oversized_frame_keeps_memory_fence_and_original_diagnostic() {
    use tokio::io::AsyncWriteExt;
    let (client, _notifications, mut peer, _root) = connection().await;
    let serving = async {
        peer.next().await.unwrap().unwrap();
        // An oversized declared frame is rejected from its header; allocate no body.
        let mut header = vec![0x81, 127];
        header.extend_from_slice(&(64_u64 * 1024 * 1024 + 1).to_be_bytes());
        peer.get_mut().write_all(&header).await.unwrap();
        // Keep the peer alive until the reader shuts down the original socket.
        let _ = peer.next().await;
    };
    let requesting = async {
        let error = client
            .thread_resume("oversized")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("67108865 > 67108864"),
            "original capacity error must survive: {error}"
        );
        assert!(client.is_closed());
        let error = client.thread_start(None).await.unwrap_err().to_string();
        assert!(
            error.contains("67108865 > 67108864"),
            "later failure must retain the first reason: {error}"
        );
        assert!(!error.contains("incomplete write"));
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(serving, requesting);
    })
    .await
    .unwrap();
}
