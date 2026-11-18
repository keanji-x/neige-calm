use super::*;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn menu(peer: &mut tokio::io::DuplexStream, session: &str, names: &[&str]) {
    let frame = json!({"jsonrpc":"2.0", "method":"session/update", "params":{
        "sessionId":session, "update":{"sessionUpdate":"available_commands_update",
        "availableCommands":names.iter().map(|name| json!({"name":name})).collect::<Vec<_>>()}}});
    peer.write_all(format!("{frame}\n").as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn command_catalog_waits_for_declaration_after_setup_response() {
    let (kernel, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new(read, write);
    let commands = Commands::default();
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let peer_release = release.clone();
    let peer_task = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        peer.get_mut()
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0", "id":request["id"], "result":{}})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        peer_release.notified().await;
        menu(peer.get_mut(), "native", &["compact"]).await;
        peer
    });
    super::super::setup_request(
        &mut connection,
        &commands,
        "session/resume",
        json!({"sessionId":"native"}),
        &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
    )
    .await
    .unwrap();
    let pending = commands.wait_for_declaration(&mut connection, "native", &["native"]);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut pending)
            .await
            .is_err()
    );
    release.notify_one();
    pending.await.unwrap();
    assert!(commands.contains("native", "compact"));
    let _peer = peer_task.await.unwrap();
}

#[tokio::test]
async fn command_catalog_wait_ignores_unbound_and_preserves_bound_sibling() {
    let (kernel, mut peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new_shared(read, write);
    let commands = Commands::default();
    menu(&mut peer, "sibling", &["compact"]).await;
    menu(&mut peer, "unbound", &["compact"]).await;
    let pending = commands.wait_for_declaration(&mut connection, "native", &["native", "sibling"]);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut pending)
            .await
            .is_err()
    );
    assert!(commands.contains("sibling", "compact"));
    assert!(!commands.declared("unbound"));
    menu(&mut peer, "native", &["compact"]).await;
    pending.await.unwrap();
    assert!(commands.contains("native", "compact"));
}

#[tokio::test]
async fn command_catalog_wait_accepts_explicit_empty_declaration() {
    let (kernel, mut peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new(read, write);
    let commands = Commands::default();
    menu(&mut peer, "native", &[]).await;
    commands
        .wait_for_declaration(&mut connection, "native", &["native"])
        .await
        .unwrap();
    assert!(commands.declared("native"));
    assert!(!commands.contains("native", "compact"));
}

#[tokio::test(start_paused = true)]
async fn command_catalog_wait_times_out_without_declaration() {
    let (kernel, _peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new(read, write);
    let commands = Commands::default();
    let start = tokio::time::Instant::now();
    assert!(matches!(
        commands
            .wait_for_declaration(&mut connection, "native", &["native"])
            .await,
        Err(CalmError::Conflict(_))
    ));
    assert_eq!(start.elapsed(), Duration::from_secs(10));
    assert!(!commands.declared("native"));
}

#[tokio::test]
async fn command_catalog_wait_refuses_disconnected_peer() {
    let (kernel, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new(read, write);
    let commands = Commands::default();
    drop(peer);
    assert!(matches!(
        commands
            .wait_for_declaration(&mut connection, "native", &["native"])
            .await,
        Err(CalmError::Conflict(_))
    ));
    assert!(!commands.declared("native"));
}

#[tokio::test]
async fn command_catalog_wait_refuses_client_requests_before_declaration() {
    let (kernel, peer) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(kernel);
    let mut connection = Connection::new(read, write);
    let commands = Commands::default();
    let mut peer = BufReader::new(peer);
    peer.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"ask\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"native\"}}\n").await.unwrap();
    let (wait, _) = tokio::join!(
        commands.wait_for_declaration(&mut connection, "native", &["native"]),
        async {
            let mut line = String::new();
            peer.read_line(&mut line).await.unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["id"], "ask");
            assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
            menu(peer.get_mut(), "native", &["compact"]).await;
        }
    );
    wait.unwrap();
}
