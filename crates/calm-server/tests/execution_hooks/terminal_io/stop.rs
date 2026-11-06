use super::*;

#[tokio::test]
async fn owner_kill_requests_typed_stop_once_before_spawn() {
    owner_kill_requests_typed_stop_once(false).await;
}

#[tokio::test]
async fn owner_kill_requests_typed_stop_once_when_live() {
    owner_kill_requests_typed_stop_once(true).await;
}

async fn owner_kill_requests_typed_stop_once(live: bool) {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (term, id) = if live {
        let (status, body) = create(&f, "", "trap '' TERM; printf ready; exec sleep 120").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let term = terminal(&f, &body).await;
        let id = execution_id(&f, &term).await;
        wait(|| async { matches!(f.engine.output(&id, None).await, Ok(ExecutionReply::Observation { state: ExecutionState::Spawned, ref output, .. }) if output == b"ready") }).await;
        (term, id)
    } else {
        // A committed intent with no drive ticket: attaching must not launch it.
        super::terminal_execution_create::projection(&f).await
    };
    // Count committed Stop requests, including redundant updates: one ClientMsg::Kill
    // must not translate into two independent Desired::Stop transactions.
    f.repo
        .db()
        .writer()
        .execute(sqlx::query(
            "CREATE TABLE stop_requests (n INTEGER NOT NULL)",
        ))
        .await
        .unwrap();
    f.repo
        .db()
        .writer()
        .execute(sqlx::query("INSERT INTO stop_requests VALUES (0)"))
        .await
        .unwrap();
    f.repo
        .db()
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER count_stop AFTER UPDATE OF desired ON workspace_executions \
        WHEN NEW.desired = 'stop' BEGIN UPDATE stop_requests SET n=n+1; END",
        ))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    let hello = ClientMsg::ClientHello {
        protocol_version: calm_session::PROTOCOL_VERSION,
        terminal_id: term.id.clone(),
        client_id: uuid::Uuid::new_v4(),
        desired_size: calm_session::PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback: calm_session::InitialScrollback::None,
        resume_from: None,
        role_hint: Some(calm_session::Role::Owner),
        capabilities: calm_session::ClientCapabilities {
            render_encodings: vec![calm_session::RenderEncoding::Vt],
            supports_scrollback: false,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    // Before Spawned, the reserved owner's hello exposes Observer until input is
    // interactive. The existing reservation still permits cancelling the launch.
    let expected_role = if live {
        calm_session::Role::Owner
    } else {
        calm_session::Role::Observer
    };
    next(&mut ws, |m| {
        matches!(m, DaemonMsg::ServerHello { client_role, .. } if *client_role == expected_role)
    })
    .await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"\n".to_vec(),
            input_seq: 1,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    next(&mut ws, |message| {
        if live {
            matches!(message, DaemonMsg::InputAck { input_seq: 1 })
        } else {
            matches!(
                message,
                DaemonMsg::ProtocolError {
                    code: calm_session::ProtocolErrorCode::NotOwner,
                    ..
                }
            )
        }
    })
    .await;
    if !live {
        assert_eq!(f.desired(&id).await, "run");
        assert!(
            f.repo
                .terminal_get(&term.id)
                .await
                .unwrap()
                .unwrap()
                .pid
                .is_none()
        );
        assert!(entry.exit.lock().unwrap().is_none());
    }
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Kill).unwrap(),
    ))
    .await
    .unwrap();
    wait(|| async { f.desired(&id).await == "stop" }).await;
    f.engine.wait_settled(&id).await.unwrap();
    // Keep the sender/renderer reachable past the legacy escalation delay.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let count: i64 = sqlx::query_scalar("SELECT n FROM stop_requests")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    let desired = f.desired(&id).await;
    f.stop(&id).await;
    server.abort();
    assert_eq!(desired, "stop");
    assert_eq!(
        count, 1,
        "one Kill must commit exactly one typed Stop request"
    );
}
