use super::terminal_execution_create::{create, execution_id, terminal};
use super::*;
use calm_session::{ClientMsg, DaemonMsg};
use calm_truth::db::RepoRead;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn http_create_ws_input_resize_stop_with_config_off() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(&f, "", "trap '' TERM; exec /bin/sh").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
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
    f.state = f.state.clone().with_terminal_execution(false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
    // An upgrade must establish the typed renderer, without a Legacy probe or EnsureProc.
    assert!(f.state.terminal_renderer.get(&term.id).is_some());
    let hello = calm_session::ClientMsg::ClientHello {
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
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();
    next(&mut ws, |m| matches!(m, DaemonMsg::ServerHello { .. })).await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 101,
            rows: 33,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    next(&mut ws, |m| matches!(m, DaemonMsg::ResizeApplied { .. })).await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"stty size; printf typed-ws-marker\\n\r".to_vec(),
            input_seq: 1,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let mut acknowledged = false;
    let mut painted = Vec::new();
    next(&mut ws, |message| {
        match message {
            DaemonMsg::InputAck { input_seq: 1 } => acknowledged = true,
            DaemonMsg::RenderPatch(patch) => painted.extend_from_slice(&patch.data),
            _ => {}
        }
        acknowledged && String::from_utf8_lossy(&painted).contains("33 101")
    })
    .await;
    assert!(
        f.state
            .terminal_renderer
            .get(&term.id)
            .unwrap()
            .exit
            .lock()
            .unwrap()
            .is_none()
    );
    ws.send(Message::Text(
        serde_json::to_string(&ClientMsg::Kill).unwrap(),
    ))
    .await
    .unwrap();
    next(&mut ws, |m| matches!(m, DaemonMsg::TerminalExited { .. })).await;
    f.engine.wait_settled(&id).await.unwrap();
    assert_eq!(f.desired(&id).await, "stop");
    assert!(
        f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .pty_output
            .is_empty()
    );
    server.abort();
}
pub(super) async fn next<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    mut predicate: impl FnMut(&DaemonMsg) -> bool,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = ws.next().await {
            if let Message::Text(text) = frame.unwrap() {
                let message: DaemonMsg = serde_json::from_str(&text).unwrap();
                if predicate(&message) {
                    return;
                }
            }
        }
        panic!("WS ended before expected frame");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn term_ignoring_shell_card_and_track_delete_first_try() {
    for delete_track in [false, true] {
        let mut f = Fixture::new().await;
        wait(|| async { f.engine.available() }).await;
        f.state = f.state.clone().with_terminal_execution(true);
        let (status, body) = create(
            &f,
            "",
            "trap '' TERM; printf ready; while :; do sleep 1; done",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let term = terminal(&f, &body).await;
        let id = execution_id(&f, &term).await;
        wait(|| async { matches!(f.engine.output(&id,None).await, Ok(ExecutionReply::Observation { ref output, .. }) if output == b"ready") }).await;
        let url = if delete_track {
            format!("/api/tracks/{}", f.track)
        } else {
            format!("/api/cards/{}", term.card_id)
        };
        let (status, body) = f.request("DELETE", &url, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        f.retired(&id).await;
        assert!(f.repo.terminal_get(&term.id).await.unwrap().is_none());
        if delete_track {
            assert!(!f.root.exists());
        }
    }
}

#[tokio::test]
async fn terminal_kind_has_no_restart_in_either_mode() {
    for typed in [false, true] {
        let mut f = Fixture::new().await;
        wait(|| async { f.engine.available() }).await;
        f.state = f.state.clone().with_terminal_execution(typed);
        let (status, body) = create(&f, "", "exec sleep 120").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let term = terminal(&f, &body).await;
        let (status, body) = f
            .request(
                "POST",
                &format!("/api/cards/{}/claude/restart", term.card_id),
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(
            f.request(
                "POST",
                &format!("/api/cards/{}/terminal/restart", term.card_id),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            f.request("DELETE", &format!("/api/cards/{}", term.card_id), None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
async fn legacy_renderer_never_claims_typed_alias() {
    let f = Fixture::new().await;
    let (_, term) = f.terminal().await;
    let id = f.admit(&term, false).await;
    let cfg = calm_server::terminal_renderer::RendererConfig {
        terminal_id: term.id.clone(),
        cols: 80,
        rows: 24,
        buffer_bytes: 8192,
        terminal_fg: (255, 255, 255),
        terminal_bg: (0, 0, 0),
        program: "/bin/true".into(),
        args: vec![],
        envs: vec![],
        cwd: f.root.to_string_lossy().into_owned(),
        supervisor_sock: f._socket.path().join("control.sock"),
    };
    let result = f.state.terminal_renderer.ensure(cfg).await;
    assert!(
        matches!(result,Err(ref error) if error.to_string().contains("typed terminal requires execution renderer"))
    );
    f.stop(&id).await;
    assert!(term.is_typed(f.repo.as_ref()).await.unwrap());
}

// Registered PTY and Drained-before-output-EOF are Linux contracts. Native Mac
// explicitly refuses registered PTY and requires complete output for settlement.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn renderer_drained_waits_for_output_eof_and_feeds_tail_before_exit() {
    use std::io::Write;
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixDatagram,
    };
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let path = f._socket.path().join("stdout.sock");
    let receiver = UnixDatagram::bind(&path).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let script = format!(
        r#"exec python3 -c 'import socket,array,os; s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM); s.connect("{}"); s.sendmsg([b"x"],[(socket.SOL_SOCKET,socket.SCM_RIGHTS,array.array("i",[1]))]); os.write(1,b"ready"); os.read(0,100)'"#,
        path.display()
    );
    let (status, body) = create(&f, "", &script).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    wait(|| async { matches!(f.engine.output(&id,None).await, Ok(ExecutionReply::Observation {ref output,..}) if output==b"ready") }).await;
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut ancillary = [0usize; 8];
    // SAFETY: aligned ancillary storage, one owned SCM_RIGHTS descriptor, CLOEXEC on receipt.
    let mut stdout = unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = ancillary.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&ancillary);
        assert_eq!(
            libc::recvmsg(receiver.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC),
            1
        );
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        std::fs::File::from_raw_fd(std::ptr::read_unaligned(
            libc::CMSG_DATA(header).cast::<i32>(),
        ))
    };
    assert!(
        f.state
            .terminal_renderer
            .get(&term.id)
            .unwrap()
            .observable()
    );
    f.state.terminal_renderer.forget_entry_for_test(&term.id);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    assert!(!entry.observable(), "cold attach has no geometry history");
    let mut events = entry.subscribe();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let ticket = f.engine.request(&mut tx, &id, Desired::Stop).await.unwrap();
    tx.commit().await.unwrap();
    f.engine.drive(ticket.committed());
    f.engine.wait_settled(&id).await.unwrap();
    assert!(matches!(
        f.engine.output(&id, None).await.unwrap(),
        ExecutionReply::Observation {
            state: ExecutionState::Drained { .. },
            output_end: OutputEnd::Open,
            ..
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), async {
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
        .is_err(),
        "Drained must not overtake output EOF"
    );
    stdout.write_all(b"late-tail").unwrap();
    drop(stdout);
    wait(|| async { entry.exit.lock().unwrap().is_some() }).await;
    let exit = entry.exit.lock().unwrap().clone().unwrap();
    {
        let plane = entry.handle.render_plane.lock().unwrap();
        assert_eq!(plane.transcript().snapshot(), b"readylate-tail");
        assert_eq!(
            exit.pty_seq,
            plane.pty_seq(),
            "exit cursor includes the final output"
        );
    }
    assert!(
        f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .pty_output
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn cold_start_recover_drives_committed_typed_with_config_off() {
    use clap::Parser;
    let f = Fixture::new().await;
    let (term, id) = super::terminal_execution_create::projection(&f).await;
    let key = f._temp.path().join("kernel-key.json");
    std::fs::write(&key, serde_json::to_vec(&f.secret).unwrap()).unwrap();
    let cfg = calm_server::config::Config::try_parse_from([
        "calm-server",
        "--kernel-client-key-file",
        key.to_str().unwrap(),
        "--proc-supervisor-sock",
        f._socket.path().join("control.sock").to_str().unwrap(),
    ])
    .unwrap();
    assert!(!cfg.terminal_execution);
    let recovered = calm_server::execution_boot::recover(
        &cfg,
        f.repo.db().clone(),
        &["claude-planner", "acp-planner"],
    )
    .await
    .unwrap()
    .unwrap();
    // Boot recovery is asynchronous; its startup work is not a deletion quiesce budget.
    wait(|| async {
        f.repo
            .terminal_get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .exit_code
            == Some(0)
    })
    .await;
    recovered.wait_settled(&id).await.unwrap();
    let terminal = f.repo.terminal_get(&term.id).await.unwrap().unwrap();
    assert_eq!(terminal.exit_code, Some(0));
    assert!(terminal.pid.is_none());
    assert_eq!(f.desired(&id).await, "run");
    assert!(terminal.is_typed(f.repo.as_ref()).await.unwrap());
    let (status, body) = f
        .request("DELETE", &format!("/api/cards/{}", term.card_id), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    f.retired(&id).await;
}

#[tokio::test]
async fn settled_worker_stopped_typed_survives_orphan_tick_and_mcp_reads_tail() {
    use calm_server::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(
        &f,
        "",
        "python3 -c 'import os; os.write(1,b\"x\"*9000+b\"END\")'; exec sleep 120",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    let original_entry = f.state.terminal_renderer.get(&term.id).unwrap();
    wait(|| async { matches!(f.engine.output(&id,None).await, Ok(ExecutionReply::Observation {ref output,..}) if output.ends_with(b"END")) }).await;
    let session: String =
        sqlx::query_scalar("SELECT id FROM worker_sessions WHERE terminal_run_id=?")
            .bind(&term.id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE tracks SET closed_at=? WHERE id=?")
        .bind(now_ms())
        .bind(&f.track)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    sqlx::query("UPDATE terminals SET created_at=0 WHERE id=?")
        .bind(&term.id)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    calm_server::terminal_sweeper::end_settled_worker_session(
        &f.state,
        &calm_server::terminal_sweeper::SettledWorkerSession {
            id: session,
            provider: "terminal".into(),
            card_id: term.card_id.to_string(),
            terminal_id: term.id.clone(),
            thread_id: None,
        },
    )
    .await
    .unwrap();
    f.engine.wait_settled(&id).await.unwrap();
    calm_server::terminal_sweeper::sweep(&f.state)
        .await
        .unwrap();
    let retained = f
        .repo
        .terminal_get(&term.id)
        .await
        .unwrap()
        .expect("signal-killed typed terminal survives orphan tick");
    assert!(retained.signal_killed);
    assert!(
        retained.pty_output.is_empty(),
        "OutputLog remains the only byte owner"
    );
    let (caller, caller_term) = f.terminal().await;
    sqlx::query("UPDATE cards SET role='planner' WHERE id=?")
        .bind(caller.id.as_str())
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    let caller_session = new_id();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: caller_session.clone(),
            card_id: caller.id.to_string(),
            kind: WorkerSessionKind::Terminal,
            agent_provider: None,
            status: WorkerSessionState::Running,
            terminal_run_id: Some(caller_term.id),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let entry = f.state.terminal_renderer.get(&term.id).unwrap();
    assert!(
        Arc::ptr_eq(&original_entry, &entry),
        "settled-worker stop retains the lifecycle owner until Retired"
    );
    wait(|| async { entry.exit.lock().unwrap().is_some() }).await;
    assert!(
        entry.observable(),
        "settled-worker retains its final screen"
    );
    f.state.terminal_renderer.drop_entry(&term.id).await;
    assert!(
        Arc::ptr_eq(&entry, &f.state.terminal_renderer.get(&term.id).unwrap()),
        "generic stop/drop cannot detach a typed execution before Retired"
    );
    f.state.terminal_renderer.forget_entry_for_test(&term.id);
    let interaction = calm_server::terminal_interaction::TerminalInteraction::new(
        f.repo.clone(),
        f.state.terminal_renderer.clone(),
        calm_server::provider_registry::WorkerProviderRegistry::from_entries([]),
    );
    let identity = calm_server::mcp_server::registry::ToolCallIdentity {
        card_id: caller.id.to_string(),
        role: CardRole::Planner,
        provider: calm_types::runtime::AgentProvider::Codex,
        session_id: caller_session,
        track_id: Some(f.track.clone()),
        area_id: f.area.clone(),
        thread_id: "card-bound".into(),
    };
    let view = interaction
        .observe(
            &identity,
            &calm_server::terminal_interaction::Target::Terminal(term.id.clone()),
            0,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(view["source"], "persisted_exit");
    assert_eq!(view["pty_output"].as_str().unwrap().len(), 8192);
    assert!(view["pty_output"].as_str().unwrap().ends_with("END"));
    assert_eq!(view["pty_output_truncated"], true);
    assert_eq!(view["controllable"], false);
    assert_eq!(f.desired(&id).await, "stop");
}

#[tokio::test]
async fn delete_rollback_retains_renderer_and_recover_retires_committed_delete() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let (status, body) = create(&f, "", "printf retained-screen; exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal(&f, &body).await;
    let id = execution_id(&f, &term).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = calm_server::ws::router().with_state(f.state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/api/terminals/{}", term.id))
            .await
            .unwrap();
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
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let deleted = sqlite::terminal_delete_tx(&mut tx, &term.id).await.unwrap();
    assert_eq!(deleted.retire, Some(id.clone()));
    tx.rollback().await.unwrap();
    assert_eq!(f.desired(&id).await, "run");
    assert!(Arc::ptr_eq(
        &entry,
        &f.state.terminal_renderer.get(&term.id).unwrap()
    ));
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
    assert!(entry.exit.lock().unwrap().is_none());
    // Commit the authoritative deletion but lose its ticket, as a crash at commit would.
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _deleted = sqlite::terminal_delete_tx(&mut tx, &term.id).await.unwrap();
    tx.commit().await.unwrap();
    assert!(f.repo.terminal_get(&term.id).await.unwrap().is_none());
    assert_eq!(f.engine.recover().await.scheduled, 1);
    f.retired(&id).await;
    wait(|| async { f.state.terminal_renderer.get(&term.id).is_none() }).await;
    wait(|| async { entry.tasks_finished_for_test() }).await;
    assert!(
        matches!(
            f.engine.output(&id, None).await.unwrap(),
            ExecutionReply::Retired(_)
        ),
        "retired execution exposes its tombstone, never output bytes"
    );
    server.abort();
}

#[tokio::test]
async fn cleanup_mode_lookup_failure_is_not_success() {
    let f = Fixture::new().await;
    let (_, term) = f.terminal().await;
    f.repo.pool().close().await;
    let result = calm_server::terminal_sweeper::try_reap_terminal_artifacts_with_renderer(
        Some(f.state.terminal_renderer.as_ref()),
        &term,
    )
    .await;
    assert!(
        result.is_err(),
        "an unavailable mode predicate must stop a deleting caller"
    );
}
