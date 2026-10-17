//! Product cookie-authenticated HTTP and WebSocket control of the same launch.
use super::*;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

pub(super) type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct HttpServer {
    pub(super) address: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl HttpServer {
    pub(super) async fn start(app: axum::Router) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
        });
        Ok(Self {
            address,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub(super) async fn post(
        &self,
        path: String,
        cookie: String,
        body: Value,
    ) -> anyhow::Result<(u16, Option<String>, Value)> {
        post_at(self.address, path, cookie, body, None).await
    }

    pub(super) async fn stop(&mut self) -> anyhow::Result<()> {
        let Some(shutdown) = self.shutdown.take() else {
            return Ok(());
        };
        let _ = shutdown.send(());
        match tokio::time::timeout(Duration::from_secs(5), &mut self.task).await {
            Ok(result) => {
                result??;
                Ok(())
            }
            Err(error) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                Err(error.into())
            }
        }
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn post_at(
    address: std::net::SocketAddr,
    path: String,
    cookie: String,
    body: Value,
    key: Option<String>,
) -> anyhow::Result<(u16, Option<String>, Value)> {
    let url = format!("http://{address}{path}");
    // The HTTP client alone blocks in this test capsule. Production async
    // authorization, preparation and the executor are exercised unchanged.
    tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(8))
            .build();
        let mut request = agent.post(&url).set("content-type", "application/json");
        if !cookie.is_empty() {
            request = request.set("cookie", &cookie);
        }
        if let Some(key) = key {
            request = request.set("Idempotency-Key", &key);
        }
        let response = match request.send_string(&body.to_string()) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(anyhow::Error::new(error)),
        };
        let status = response.status();
        let cookie = response
            .header("set-cookie")
            .map(|value| value.split(';').next().unwrap().to_owned());
        let body = serde_json::from_str(&response.into_string()?)?;
        Ok((status, cookie, body))
    })
    .await?
}

pub(super) async fn login(server: &HttpServer) -> anyhow::Result<String> {
    let (status, cookie, _) = server
        .post(
            "/api/auth/login".into(),
            String::new(),
            json!({"username":"owner","password":"terminal-password"}),
        )
        .await?;
    ensure!(status == 200, "real HTTP login failed: {status}");
    cookie.context("real HTTP login supplied no cookie")
}

pub(super) async fn send(socket: &mut Socket, message: ClientMsg) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(&message)?))
        .await?;
    Ok(())
}

pub(super) async fn until(
    socket: &mut Socket,
    mut matches: impl FnMut(&DaemonMsg) -> bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match socket
                .next()
                .await
                .context("product WebSocket closed before expected result")??
            {
                Message::Text(text) => {
                    let message: DaemonMsg = serde_json::from_str(&text)?;
                    if matches(&message) {
                        return Ok(());
                    }
                    if let DaemonMsg::ProtocolError { code, message, .. } = message {
                        anyhow::bail!("product protocol rejected request: {code:?} {message}");
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
                Message::Close(reason) => anyhow::bail!("product WebSocket closed: {reason:?}"),
                _ => {}
            }
        }
    })
    .await?
}

pub(super) async fn disconnect(
    mut socket: Socket,
    entry: Arc<calm_server::terminal_renderer::RendererEntry>,
) -> anyhow::Result<()> {
    socket.close(None).await?;
    drop(socket);
    // Wait for the real product connection guard to release this owner lease;
    // no synthetic release frame or fixture-side ownership mutation is used.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if entry
                .handle
                .owner_registry
                .lock()
                .unwrap()
                .current_owner()
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

pub(super) async fn input(
    socket: &mut Socket,
    sequence: u64,
    text: &str,
    expected: &[u8],
) -> anyhow::Result<()> {
    send(
        socket,
        ClientMsg::Input {
            data: format!("{text}\r").into_bytes(),
            input_seq: sequence,
        },
    )
    .await?;
    let mut acknowledged = false;
    let mut output = Vec::new();
    until(socket, |message| {
        match message {
            DaemonMsg::InputAck { input_seq } if *input_seq == sequence => acknowledged = true,
            DaemonMsg::RenderPatch(patch) => output.extend_from_slice(&patch.data),
            DaemonMsg::RenderSnapshot(snapshot) => output.extend_from_slice(&snapshot.data),
            _ => {}
        }
        acknowledged
            && output
                .windows(expected.len())
                .any(|bytes| bytes == expected)
    })
    .await
}

pub(super) fn install_interactive_program(fixture: &Fixture) -> anyhow::Result<()> {
    std::fs::write(
        &fixture.program,
        r#"#!/bin/sh
trap '' HUP TERM
printf 'executed\n' >> "$NEIGE_TERMINAL_VERTICAL_COUNT"
while IFS= read -r line; do
  if [ "$line" = size ]; then stty size; else printf 'received:%s\n' "$line"; fi
done
"#,
    )?;
    Ok(())
}

pub(super) async fn open(
    server: &HttpServer,
    cookie: &str,
    terminal: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<Socket> {
    let mut request =
        format!("ws://{}/api/terminals/{}", server.address, terminal).into_client_request()?;
    request.headers_mut().insert("cookie", cookie.parse()?);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
    send(
        &mut socket,
        ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal.to_owned(),
            client_id: uuid::Uuid::new_v4(),
            desired_size: PtySize {
                cols,
                rows,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: Some(Role::Owner),
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        },
    )
    .await?;
    until(&mut socket, |message| {
        matches!(
            message,
            DaemonMsg::ServerHello {
                client_role: Role::Owner,
                ..
            }
        )
    })
    .await?;
    Ok(socket)
}

async fn exercise(fixture: &mut Fixture, server: &HttpServer) -> anyhow::Result<()> {
    install_interactive_program(fixture)?;
    let (status, cookie, _) = server
        .post(
            "/api/auth/login".into(),
            String::new(),
            json!({"username":"owner","password":"terminal-password"}),
        )
        .await?;
    ensure!(status == 200, "real HTTP login failed: {status}");
    let cookie = cookie.context("real HTTP login supplied no cookie")?;
    let (status, _, card) = server
        .post(
            format!("/api/tracks/{}/terminal-cards", fixture.track),
            cookie.clone(),
            json!({
                "program":format!("'{}'",fixture.program.to_str().unwrap().replace('\'',"'\\''")),
                "env":{"NEIGE_TERMINAL_VERTICAL_COUNT":fixture.count},
                "theme":calm_server::routes::theme::RequestTheme::default_dark(),
            }),
        )
        .await?;
    ensure!(
        status == 201,
        "real HTTP Terminal launch failed: {status} {card}"
    );
    let terminal = fixture
        .sql
        .terminal_get_by_card(card["id"].as_str().context("missing card id")?)
        .await?
        .context("missing real Terminal")?;
    let mut socket = open(server, &cookie, &terminal.id, 80, 24).await?;
    send(&mut socket, ClientMsg::OwnerClaim).await?;
    send(
        &mut socket,
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 113,
            rows: 37,
        },
    )
    .await?;
    until(&mut socket, |message| {
        matches!(
            message,
            DaemonMsg::ResizeApplied {
                epoch: 1,
                cols: 113,
                rows: 37,
                ..
            }
        )
    })
    .await?;
    input(&mut socket, 1, "size", b"37 113").await?;
    input(
        &mut socket,
        2,
        "registered-input",
        b"received:registered-input",
    )
    .await?;
    ensure!(
        fixture
            .state
            .terminal_renderer
            .get(&terminal.id)
            .context("product renderer disappeared")?
            .observable(),
        "fresh complete initial geometry should remain observable"
    );
    send(&mut socket, ClientMsg::Kill).await?;
    until(&mut socket, |message| {
        matches!(message, DaemonMsg::TerminalExited { .. })
    })
    .await?;
    let _ = socket.close(None).await;
    let request_json: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
        .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
    let request: PrepareExecution =
        serde_json::from_value(serde_json::from_str::<Value>(&request_json)?["request"].clone())?;
    let journal: Value = serde_json::from_slice(&std::fs::read(fixture.journal.join(format!(
        "{}.execution",
        request.identity.execution_id.as_str()
    )))?)?;
    ensure!(
        journal["phase"].get("Drained").is_some(),
        "product Kill/TerminalExited did not positively settle its real executor scope"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_http_ws_input_resize_and_kill_use_original_registered_execution()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start().await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let result = exercise(&mut fixture, &server).await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "product WS control launched another execution"
    );
    Ok(())
}
