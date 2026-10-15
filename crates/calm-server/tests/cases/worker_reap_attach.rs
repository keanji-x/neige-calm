//! Final-screen attachment and persisted-exit authorization after a worker reap.
use super::*;

#[tokio::test]
async fn worker_reap_fresh_ws_attach_keeps_final_screen() {
    use calm_session::{
        ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
        RenderEncoding,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    for closed in [false, true] {
        let h = Harness::start().await;
        let p = open_sleeper(&h, "final-screen").await;
        attempt(&h, &p, "final-screen", 1, "done", Some(120_000)).await;
        if closed {
            complete_track(&h, &h.track).await;
        }
        calm_server::terminal_sweeper::sweep(&h.state)
            .await
            .unwrap();
        assert_reaped(&h, &p).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = calm_server::ws::terminal::router().with_state(h.state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("ws://{addr}/api/terminals/{}", p.terminal);
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let hello = ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: p.terminal.clone(),
            client_id: uuid::Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::All,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        };
        ws.send(Message::Text(serde_json::to_string(&hello).unwrap()))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(frame, Message::Text(_)),
            "expected final-screen hello, got {frame:?}"
        );
        let msg: DaemonMsg = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        let DaemonMsg::ServerHello { snapshot, .. } = msg else {
            panic!("missing final-screen hello: {msg:?}")
        };
        assert!(String::from_utf8_lossy(&snapshot.data).contains("READY"));
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let msg: DaemonMsg = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert!(matches!(msg, DaemonMsg::TerminalExited { .. }), "{msg:?}");
        let refused = h.call("neige_terminal_input", json!({"terminal_id": p.terminal, "idempotency_key":"after-exit", "action":{"type":"submit", "text":"must-not-run"}})).await;
        assert_eq!(refused["error"]["code"], -32403, "{refused}");
        let retained = h.state.terminal_renderer.get(&p.terminal).unwrap();
        let ensured = h
            .state
            .terminal_renderer
            .ensure(retained.config().clone())
            .await
            .unwrap();
        assert!(
            Arc::ptr_eq(&retained, &ensured),
            "ensure must reuse the ended entry"
        );
        assert!(
            retained
                .wait_exit_persisted_for_test(Duration::from_secs(1))
                .await
        );
        server.abort();
        assert!(!pid_alive(p.pid));
        assert_eq!(
            h.state
                .repo
                .terminal_get(&p.terminal)
                .await
                .unwrap()
                .unwrap()
                .pid,
            Some(p.pid)
        );
        h.stop(&p.terminal).await;
    }
}

#[tokio::test]
async fn worker_reap_persisted_exit_read_authorization() {
    use crate::terminal_ws_suite::assistant_terminal::{agent_token, foreign_track};
    let h = Harness::start().await;
    let p = open_sleeper(&h, "private-exit").await;
    complete_track(&h, &h.track).await;
    calm_server::terminal_sweeper::sweep(&h.state)
        .await
        .unwrap();
    assert_reaped(&h, &p).await;
    let view = h
        .ok("neige_terminal_read", json!({"terminal_id":p.terminal}))
        .await;
    assert_eq!(view["source"], "persisted_exit", "{view}");
    let foreign = foreign_track(&h).await;
    let (_, _, foreign_token) = agent_token(&h, &foreign, CardRole::Planner).await;
    let (_, _, assistant_token) = agent_token(&h, &h.track, CardRole::Assistant).await;
    for (token, reason) in [
        (foreign_token, "outside the caller's Track"),
        (assistant_token, "an Assistant reaches only task workers"),
    ] {
        let denied = h
            .call_with_token(
                &token,
                "neige_terminal_read",
                json!({"terminal_id":p.terminal}),
            )
            .await;
        assert_eq!(denied["error"]["code"], -32403, "{denied}");
        assert!(
            denied["error"]["message"]
                .as_str()
                .unwrap()
                .contains(reason),
            "{denied}"
        );
        assert!(
            denied["result"].is_null(),
            "must not disclose exit output: {denied}"
        );
    }
    h.stop(&p.terminal).await;
}
