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

enum ResumeCall {
    Plain,
    Configured,
    Gated,
}

async fn resume_without_history_keeps_catalog(call: ResumeCall) {
    use tokio::io::AsyncWriteExt;

    let (client, _notifications, mut peer, _root) = connection().await;
    let serving = async {
        while let Some(Ok(Message::Text(raw))) = peer.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "thread/resume" => {
                    if request["params"]["excludeTurns"] != true {
                        // Codex returns the full history unless explicitly excluded.
                        // Model an over-budget response using only its frame header.
                        let mut header = vec![0x81, 127];
                        header.extend_from_slice(&(64_u64 * 1024 * 1024 + 1).to_be_bytes());
                        peer.get_mut().write_all(&header).await.unwrap();
                        let _ = peer.next().await;
                        std::future::pending::<()>().await;
                        return;
                    }
                    json!({"thread":{"id":"long-thread","status":{"type":"idle"},"turns":[]},"model":"catalog-model"})
                }
                "model/list" => {
                    json!({"data":[{"id":"catalog-model","model":"catalog-model"}],"nextCursor":null})
                }
                method => panic!("unexpected method: {method}"),
            };
            peer.send(Message::Text(
                json!({"id":request["id"],"result":result}).to_string(),
            ))
            .await
            .unwrap();
        }
    };
    let requesting = async {
        let deadline = || tokio::time::Instant::now() + Duration::from_secs(2);
        let before = client.model_list(None, deadline()).await.unwrap();
        assert_eq!(before.data[0]["model"], "catalog-model");
        let config = Some(json!({"model_reasoning_effort":"high"}));
        let result = match call {
            ResumeCall::Plain => client.thread_resume("long-thread").await,
            ResumeCall::Configured => {
                client
                    .thread_resume_with_config("long-thread", config)
                    .await
            }
            ResumeCall::Gated => {
                client
                    .thread_resume_with_config_gated(
                        "long-thread",
                        config,
                        &PreSendGate::new(gated_requests::allow_send),
                    )
                    .await
            }
        };
        let resumed = result.expect("resuming must not download an oversized retained history");
        assert_eq!(resumed.thread_id(), Some("long-thread"));
        assert_eq!(resumed.thread["status"]["type"], "idle");
        let after = client.model_list(None, deadline()).await.unwrap();
        assert_eq!(
            after.data, before.data,
            "resume must preserve the model catalog"
        );
        assert!(!client.is_closed());
    };
    // The peer is deliberately persistent; stop it only after the client checks.
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = serving => panic!("peer closed before the catalog checks finished"),
            () = requesting => {}
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn plain_resume_without_history_preserves_model_catalog() {
    resume_without_history_keeps_catalog(ResumeCall::Plain).await;
}

#[tokio::test]
async fn configured_resume_without_history_preserves_model_catalog() {
    resume_without_history_keeps_catalog(ResumeCall::Configured).await;
}

#[tokio::test]
async fn gated_resume_without_history_preserves_model_catalog() {
    resume_without_history_keeps_catalog(ResumeCall::Gated).await;
}

#[tokio::test]
async fn disconnect_notification_stream_retains_capacity_failure() {
    use tokio::io::AsyncWriteExt;
    let (_client, mut notifications, mut peer, _root) = connection().await;
    let mut header = vec![0x81, 127];
    header.extend_from_slice(&(64_u64 * 1024 * 1024 + 1).to_be_bytes());
    peer.get_mut().write_all(&header).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), notifications.recv_result())
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("67108865 > 67108864"),
        "notification path lost the original failure: {error}"
    );
}

#[tokio::test]
async fn disconnect_peer_close_preserves_code_and_drains_notifications_without_peer_text() {
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
    let (client, mut notifications, mut peer, _root) = connection().await;
    peer.send(Message::Text(
        json!({"method":"test/queued", "params":{}}).to_string(),
    ))
    .await
    .unwrap();
    peer.send(Message::Close(Some(CloseFrame {
        code: CloseCode::Away,
        reason: "secret peer text".into(),
    })))
    .await
    .unwrap();
    assert!(
        matches!(notifications.recv_result().await.unwrap(), Notification::Other {method, ..} if method=="test/queued")
    );
    let error = tokio::time::timeout(Duration::from_secs(2), notifications.recv_result())
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("1001"), "close code must survive: {error}");
    assert!(!error.contains("secret peer text"));
    client.transport.poison();
    assert_eq!(
        notifications.recv_result().await.unwrap_err().to_string(),
        error
    );
}

#[tokio::test]
async fn disconnect_aborted_reader_notifies_consumers_without_hanging() {
    let (client, mut notifications, _peer, _root) = connection().await;
    client.reader.abort();
    let error = tokio::time::timeout(Duration::from_secs(2), notifications.recv_result())
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("reader or dispatch owner dropped"),
        "reader cancellation must have its own reason: {error}"
    );
}
