use super::terminal_io::next;
use super::*;
use calm_server::terminal_renderer::refusal_test_hook::{self, Stage};
use calm_session::{ClientMsg, DaemonMsg, ProtocolErrorCode};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn pre_ready_refusal_enqueue_across_spawn_two_clients() {
    refusal_across_spawn(Stage::Enqueue).await;
}

#[tokio::test]
async fn pre_ready_refusal_delivery_across_spawn_two_clients() {
    refusal_across_spawn(Stage::Delivery).await;
}

async fn refusal_across_spawn(stage: Stage) {
    let f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    let (_, term) = f.terminal().await;
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.track = exec::TrackId(f.track.clone());
    intent.metadata.alias = ProcessAlias(sql::terminal_alias(&term.id));
    intent.metadata.program.io_mode = calm_session::control::IoMode::Pty { cols: 80, rows: 24 };
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec!["-c".into(), "exec /bin/sh".into()];
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut clients = Vec::new();
    let mut holds = Vec::new();
    for _ in 0..2 {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
                .await
                .unwrap();
        let client_id = uuid::Uuid::new_v4();
        // Install before hello starts the downstream select. Hold only the direct branch;
        // broadcasts remain deliverable while refusals accumulate in its queue.
        if matches!(stage, Stage::Delivery) {
            let (hold, _entered) = refusal_test_hook::install(client_id, Stage::Delivery);
            holds.push(hold);
        }
        let hello = ClientMsg::ClientHello {
            protocol_version: calm_session::PROTOCOL_VERSION,
            terminal_id: term.id.clone(),
            client_id,
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
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        };
        ws.send(Message::Text(serde_json::to_string(&hello).unwrap()))
            .await
            .unwrap();
        next(&mut ws, |m| {
            matches!(
                m,
                DaemonMsg::ServerHello {
                    client_role: calm_session::Role::Observer,
                    owner_client_id: None,
                    ..
                }
            )
        })
        .await;
        clients.push((client_id, ws));
    }
    for (index, (client, ws)) in clients.iter_mut().enumerate() {
        let (hold, entered) = refusal_test_hook::install(
            *client,
            if matches!(stage, Stage::Delivery) {
                Stage::Enqueued
            } else {
                Stage::Enqueue
            },
        );
        // Exercise both refused production entry points on separate connections.
        let msg = if index == 0 {
            ClientMsg::Input {
                data: b"printf PRE_READY_LEAK > pre-ready-leak\r".to_vec(),
                input_seq: 1,
            }
        } else {
            ClientMsg::OwnerClaim
        };
        ws.send(Message::Text(serde_json::to_string(&msg).unwrap()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        holds.push(hold);
    }
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    let mut events = entry.subscribe();
    f.engine.drive(ticket.committed());
    let owner = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let DaemonMsg::OwnerChanged {
                owner_client_id: Some(owner),
            } = events.recv().await.unwrap()
            {
                break owner;
            }
        }
    })
    .await
    .unwrap();
    // In both schedules readiness overtakes the refusal: either the refusal is not yet
    // enqueued, or it is queued but the direct receive branch is held across Spawned.
    for (_, ws) in &mut clients {
        next(ws, |m| matches!(m, DaemonMsg::OwnerChanged { owner_client_id: Some(current) } if *current == owner)).await;
    }
    drop(holds);
    for (_, ws) in &mut clients {
        let mut refused = false;
        next(ws, |m| {
            assert!(
                !matches!(m, DaemonMsg::InputAck { .. }),
                "pre-ready bytes are unacknowledged"
            );
            match m {
                DaemonMsg::ProtocolError {
                    code: ProtocolErrorCode::NotOwner,
                    ..
                } => refused = true,
                DaemonMsg::OwnerChanged {
                    owner_client_id: Some(current),
                } if refused => {
                    assert_eq!(*current, owner, "recovery names the authoritative owner");
                    return true;
                }
                _ => {}
            }
            false
        })
        .await;
        assert!(refused);
    }
    for (index, (client, ws)) in clients.iter_mut().enumerate() {
        // The original owner needs no further claim or manual recovery. The observer
        // deliberately takes control only to exercise its own post-ready input path.
        if *client != owner {
            ws.send(Message::Text(
                serde_json::to_string(&ClientMsg::OwnerClaim).unwrap(),
            ))
            .await
            .unwrap();
            next(ws, |m| matches!(m, DaemonMsg::OwnerChanged { owner_client_id: Some(current) } if current == client)).await;
        }
        let input_seq = 10 + index as u64;
        ws.send(Message::Text(
            serde_json::to_string(&ClientMsg::Input {
                data: format!("printf POST_READY_{index}\\n\r").into_bytes(),
                input_seq,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        next(
            ws,
            |m| matches!(m, DaemonMsg::InputAck { input_seq: seq } if *seq == input_seq),
        )
        .await;
    }
    // A later acknowledged shell command is a child-side fence for all earlier input.
    let (_, ws) = clients.last_mut().unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"printf done > post-ready-fence\r".to_vec(),
            input_seq: 20,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    next(ws, |m| matches!(m, DaemonMsg::InputAck { input_seq: 20 })).await;
    wait(|| async { f.root.join("post-ready-fence").exists() }).await;
    assert!(
        !f.root.join("pre-ready-leak").exists(),
        "refused input must never reach the child or replay"
    );
    f.stop(&id).await;
    server.abort();
}
