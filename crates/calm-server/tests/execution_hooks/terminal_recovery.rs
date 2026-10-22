use super::terminal_execution_create::{create, execution_id, terminal};
use super::terminal_io::next;
use super::*;
use calm_session::{ClientMsg, DaemonMsg};
use calm_truth::db::RepoRead;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn card_delete_rollback_retains_typed_renderer() {
    delete_rollback("cards").await;
}
#[tokio::test]
async fn track_delete_rollback_retains_typed_renderer() {
    delete_rollback("tracks").await;
}
#[tokio::test]
async fn area_delete_rollback_retains_typed_renderer() {
    delete_rollback("areas").await;
}
#[tokio::test]
async fn sweeper_delete_rollback_retains_typed_renderer() {
    delete_rollback("sweeper").await;
}

async fn delete_rollback(scope: &str) {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(&f, "", "printf retained-screen; exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    wait(|| async {
        entry
            .handle
            .render_plane
            .lock()
            .unwrap()
            .transcript()
            .snapshot()
            == b"retained-screen"
    })
    .await;
    let mut subscription = entry.subscribe();
    sqlx::query("CREATE TRIGGER reject_delete BEFORE DELETE ON terminals BEGIN SELECT RAISE(ABORT, 'later delete failed'); END")
            .execute(f.repo.db().writer_pool()).await.unwrap();
    if scope == "sweeper" {
        sqlx::query("UPDATE worker_sessions SET state='exited' WHERE terminal_run_id=?")
            .bind(&term.id)
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        sqlx::query("UPDATE terminals SET created_at=0 WHERE id=?")
            .bind(&term.id)
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        calm_server::terminal_sweeper::sweep(&f.state)
            .await
            .unwrap();
    } else {
        let key = match scope {
            "cards" => term.card_id.as_str(),
            "tracks" => &f.track,
            _ => &f.area,
        };
        let (status, body) = f
            .request("DELETE", &format!("/api/{scope}/{key}"), None)
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(body.contains("later delete failed"), "{body}");
    }
    assert_eq!(
        f.desired(&id).await,
        "stop",
        "quiesce committed, delete rolled back"
    );
    assert!(f.repo.terminal_get(&term.id).await.unwrap().is_some());
    assert!(
        f.repo
            .card_get(term.card_id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(Arc::ptr_eq(
        &entry,
        &f.state
            .terminal_renderer
            .get(&term.id)
            .expect("rollback retains renderer")
    ));
    wait(|| async { entry.exit.lock().unwrap().is_some() }).await;
    assert_eq!(
        entry
            .handle
            .render_plane
            .lock()
            .unwrap()
            .transcript()
            .snapshot(),
        b"retained-screen"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                subscription.recv().await.unwrap(),
                DaemonMsg::TerminalExited { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        Arc::ptr_eq(&entry, &f.state.terminal_renderer.get(&term.id).unwrap()),
        "Drained retains the same final screen until Retired"
    );
    sqlx::query("DROP TRIGGER reject_delete")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.stop(&id).await;
}

#[tokio::test]
async fn ws_before_spawn_retains_latest_resize_and_recovers_input() {
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
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
    let client_id = uuid::Uuid::new_v4();
    let hello = calm_session::ClientMsg::ClientHello {
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
        if let DaemonMsg::ServerHello {
            client_role,
            owner_client_id,
            ..
        } = m
        {
            assert_eq!(*client_role, calm_session::Role::Observer);
            assert!(owner_client_id.is_none(), "no input owner before Spawned");
            true
        } else {
            false
        }
    })
    .await;
    // NotOwner is recoverable by the shipped browser; BadSequence disables it.
    for epoch in 1..=2 {
        ws.send(Message::Text(
            serde_json::to_string(&ClientMsg::ResizeCommit {
                epoch,
                cols: 90 + epoch as u16,
                rows: 30,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        next(&mut ws, |m| matches!(m, DaemonMsg::ResizeApplied { .. })).await;
        ws.send(Message::Text(
            serde_json::to_string(&ClientMsg::Input {
                data: b"ignored\r".to_vec(),
                input_seq: epoch.into(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        next(&mut ws, |m| {
            assert!(
                !matches!(m, DaemonMsg::InputAck { .. }),
                "refused input must not be acknowledged"
            );
            matches!(
                m,
                DaemonMsg::ProtocolError {
                    code: calm_session::ProtocolErrorCode::NotOwner,
                    ..
                }
            )
        })
        .await;
    }
    f.engine.drive(ticket.committed());
    wait(|| async {
        matches!(
            f.engine.output(&id, None).await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Spawned,
                ..
            })
        )
    })
    .await;
    // The browser restores stdin only on an ownership acknowledgement after NotOwner.
    next(&mut ws, |m| {
        matches!(
            m,
            DaemonMsg::OwnerChanged {
                owner_client_id: Some(owner)
            } if *owner == client_id
        )
    })
    .await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"stty size\r".to_vec(),
            input_seq: 3,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let mut ack = false;
    let mut output = Vec::new();
    next(&mut ws, |m| {
        match m {
            DaemonMsg::InputAck { input_seq: 3 } => ack = true,
            DaemonMsg::RenderPatch(p) => output.extend_from_slice(&p.data),
            _ => {}
        }
        ack && String::from_utf8_lossy(&output).contains("30 92")
    })
    .await;
    f.stop(&id).await;
    server.abort();
}

#[tokio::test]
async fn observe_transport_loss_resumes_confirmed_cursor_and_exit() {
    use calm_session::control::{ControlMsg, ControlReply, execution::rights};
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(
        &f,
        "",
        "printf before-loss; read first; printf during-loss; read second; printf after-loss",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    wait(|| async {
        entry
            .handle
            .render_plane
            .lock()
            .unwrap()
            .transcript()
            .snapshot()
            == b"before-loss"
    })
    .await;
    let mut events = entry.subscribe();
    // Interpose only after launch (no source FD transfer): fail exactly one real Observe
    // command while leaving executor, child and existing Watch stream alive.
    let socket = f.state.daemon.proc_supervisor_sock.as_ref().unwrap();
    let backend = socket.with_extension("backend");
    std::fs::rename(socket, &backend).unwrap();
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let injected = failed.clone();
    let proxy = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let backend = backend.clone();
            let failed = injected.clone();
            connections.spawn(async move {
                let mut server = tokio::net::UnixStream::connect(backend).await.unwrap();
                let hello: ControlMsg = calm_session::read_frame(&mut client).await.unwrap();
                calm_session::write_frame(&mut server, &hello).await.unwrap();
                let hello: ControlReply = calm_session::read_frame(&mut server).await.unwrap();
                calm_session::write_frame(&mut client, &hello).await.unwrap();
                let (request, files) = rights::receive::<ControlMsg>(&client).await.unwrap();
                assert!(files.is_empty());
                if matches!(&request, ControlMsg::Execution(r) if matches!(r.as_ref(), ExecutionRequest::Control(c) if matches!(c.action, ProcessAction::Observe { .. })))
                    && !failed.swap(true, Ordering::SeqCst)
                {
                    return; // EOF instead of Observe reply: the adapter must rebuild Watch.
                }
                rights::send(&server, &request, &[]).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            });
        }
    });
    // A real output notification triggers the reader's command through the proxy.
    f.engine
        .interact(
            &id,
            ProcessAction::Resize {
                cols: 81,
                rows: 24,
                pixel_w: 0,
                pixel_h: 0,
            },
        )
        .await
        .unwrap();
    // Resize does not promise Changed; stdin prints another chunk without ending the child.
    f.engine
        .interact(
            &id,
            ProcessAction::WriteStdin {
                bytes: b"\n".to_vec(),
            },
        )
        .await
        .unwrap();
    wait(|| async { failed.load(Ordering::SeqCst) }).await;
    assert!(
        entry.exit.lock().unwrap().is_none(),
        "child is alive at transport failure"
    );
    f.engine
        .interact(
            &id,
            ProcessAction::WriteStdin {
                bytes: b"\n".to_vec(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                events.recv().await.unwrap(),
                DaemonMsg::TerminalExited { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let output = entry
        .handle
        .render_plane
        .lock()
        .unwrap()
        .transcript()
        .snapshot();
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("before-loss")
            .count(),
        1
    );
    assert!(String::from_utf8_lossy(&output).contains("after-loss"));
    assert!(
        entry.observable(),
        "read-only transport loss cannot invalidate a complete screen"
    );
    f.stop(&id).await;
    proxy.abort();
}

#[tokio::test]
async fn ended_typed_reader_is_replaced_on_attach() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(&f, "", "printf before-disconnect; exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    let old = f.state.terminal_renderer.get(&term.id).unwrap();
    old.disconnect_output_source_for_test();
    wait(|| async { f.state.terminal_renderer.get(&term.id).is_none() }).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
    let new = f.state.terminal_renderer.get(&term.id).unwrap();
    assert!(
        !Arc::ptr_eq(&old, &new),
        "attach must not return a reader that ended unexpectedly"
    );
    wait(|| async { old.tasks_finished_for_test() }).await;
    f.stop(&id).await;
    server.abort();
}
