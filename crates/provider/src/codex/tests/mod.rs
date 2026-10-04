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
    let (client_io, server_io) = UnixStream::pair().expect("unix socket pair");

    // Drive both handshakes concurrently.
    let req = WS_URI.into_client_request().unwrap();
    let client_fut = tokio_tungstenite::client_async(req, client_io);
    let server_fut = tokio_tungstenite::accept_async(server_io);
    let (client_res, server_res) = tokio::join!(client_fut, server_fut);
    let (client_ws, _resp) = client_res.expect("client handshake");
    let server = server_res.expect("server handshake");

    let transport =
        Arc::new(TransportAbort::new(client_ws.get_ref()).expect("retain owned socket"));
    let (write, read) = client_ws.split();
    let sink: WsSink = Arc::new(Mutex::new(write));
    let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
    let (notif_tx, notif_rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(reader_loop(
        read,
        pending.clone(),
        notif_tx,
        sink.clone(),
        transport.clone(),
    ));

    let client = CodexAppServer {
        sink,
        transport,
        pending,
        next_id: AtomicU64::new(1),
        request_timeout: DEFAULT_REQUEST_TIMEOUT,
        reader,
    };
    Harness {
        client,
        _notifs: NotificationStream { rx: notif_rx },
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
