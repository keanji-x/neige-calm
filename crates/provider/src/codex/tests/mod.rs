use super::*;

/// A real WS-over-UnixStream connection: a fully-constructed client on one end and a raw [`WebSocketStream`] the test drives on the other, no `codex` binary.
struct Harness {
    client: CodexAppServer,
    /// Must stay alive even when not drained: dropping it closes the channel and stops the reader.
    _notifs: NotificationStream,
    /// Server-side WS end; the test reads requests off it and writes responses back.
    server: WebSocketStream<UnixStream>,
}

async fn harness() -> Harness {
    let (client, notifications, server) = CodexAppServer::connect_pair_for_test().await;
    Harness {
        client,
        _notifs: notifications,
        server,
    }
}

/// Pull the next text frame off the server end as parsed JSON.
async fn server_recv_json(server: &mut WebSocketStream<UnixStream>) -> Value {
    loop {
        match server.next().await.expect("frame").expect("ws ok") {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Close(_) => panic!("server saw close before a request"),
            _ => continue,
        }
    }
}

async fn server_send_json(server: &mut WebSocketStream<UnixStream>, v: Value) {
    server
        .send(Message::Text(serde_json::to_string(&v).unwrap()))
        .await
        .expect("server send");
}
mod group_1;
mod group_2;
mod request_attempt;

mod delivery;
