//! Ordinary terminal WS over a real isolated shell; no model or application event wakeups.
use super::*;
use crate::terminal_ws_suite::terminal_support::Harness;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn recv(ws: &mut Socket) -> DaemonMsg {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}
async fn send(ws: &mut Socket, msg: ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap()))
        .await
        .unwrap();
}
async fn connect(
    address: std::net::SocketAddr,
    terminal: &str,
    id: Option<&str>,
    cookie: &str,
) -> Socket {
    let selector = id.map(|id| format!("?share_link={id}")).unwrap_or_default();
    let mut req = format!("ws://{address}/api/terminals/{terminal}{selector}")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
    send(
        &mut ws,
        ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal.into(),
            client_id: uuid::Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        },
    )
    .await;
    let hello = recv(&mut ws).await;
    assert!(
        matches!(
            hello,
            DaemonMsg::ServerHello {
                client_role: Role::Owner,
                ..
            }
        ),
        "{hello:?}"
    );
    ws
}
async fn input(ws: &mut Socket, command: &str, seq: u64) {
    send(
        ws,
        ClientMsg::Input {
            data: format!("{command}\n").into_bytes(),
            input_seq: seq,
        },
    )
    .await;
}
async fn ack(ws: &mut Socket, seq: u64) {
    loop {
        if matches!(recv(ws).await, DaemonMsg::InputAck { input_seq } if input_seq == seq) {
            return;
        }
    }
}
async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn shell() -> (Harness, Fixture, String) {
    let h = Harness::start().await;
    let opened = h
        .ok(
            "neige_terminal_open",
            json!({"program":"/bin/sh", "idempotency_key":"share-shell", "claim":false}),
        )
        .await;
    let terminal = opened["terminal_id"].as_str().unwrap().to_owned();
    let mut fx = Fixture::with_state(h.state.clone()).await;
    fx.track = h.track.clone();
    (h, fx, terminal)
}

async fn invalidation(replace: bool, queued: bool) {
    let (h, fx, terminal) = shell().await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let (address, server) = super::scoped_events::serve(&fx).await;
    let mut ws = connect(address, &terminal, Some(id), &cookie).await;
    let healthy = h.root.path().join("healthy");
    input(&mut ws, &format!("printf ok > {}", healthy.display()), 1).await;
    ack(&mut ws, 1).await;
    wait_file(&healthy).await;
    let marker = h.root.path().join("forbidden");
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let barrier = if queued {
        Some(entry.handle.input_barrier.hold_for_test().await)
    } else {
        None
    };
    let pause = calm_server::test_seams::PausePoint::default();
    if queued {
        calm_server::test_seams::install_pause_for_test(
            "terminal-input-enqueued",
            &terminal,
            pause.clone(),
        );
        input(
            &mut ws,
            &format!("printf forbidden > {}", marker.display()),
            2,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
            .await
            .unwrap();
    }
    let recheck = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "share-terminal-before-recheck",
        &terminal,
        recheck.clone(),
    );
    let replacement = if replace {
        let response = fx
            .app
            .clone()
            .oneshot(
                Request::post(format!("/api/share-links/{id}/unlock"))
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"token":link["token"]}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        Some(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
        )
    } else {
        assert_eq!(
            fx.request(
                "POST",
                &format!("/api/auth/logout?share_link={id}"),
                Some(&cookie),
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        );
        None
    };
    assert_eq!(
        fx.request(
            "GET",
            &format!("/api/tracks/{}?share_link={id}", fx.track),
            Some(&cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    tokio::time::timeout(Duration::from_secs(5), recheck.entered.notified())
        .await
        .unwrap();
    if queued {
        pause.release.notify_one();
    } else {
        input(
            &mut ws,
            &format!("printf forbidden > {}", marker.display()),
            2,
        )
        .await;
    }
    drop(barrier);
    // Keep the connection and its lease alive while the actual writer rejects
    // the input. Notification/close alone cannot satisfy this assertion.
    loop {
        let msg = recv(&mut ws).await;
        assert!(
            !matches!(msg, DaemonMsg::InputAck { input_seq: 2 }),
            "revoked input was acknowledged"
        );
        if matches!(
            msg,
            DaemonMsg::ProtocolError {
                code: calm_session::ProtocolErrorCode::NotOwner,
                ..
            }
        ) {
            break;
        }
    }
    recheck.release.notify_one();
    // Drain terminal output to the close, rejecting any successful ACK for the forbidden input.
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Close(_) => break,
                Message::Text(text) => assert!(!matches!(
                    serde_json::from_str::<DaemonMsg>(&text),
                    Ok(DaemonMsg::InputAck { input_seq: 2 })
                )),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    // The writer is serial: this healthy Owner command proves the old queued write has settled.
    let mut owner = connect(address, &terminal, None, &fx.cookie).await;
    let settled = h.root.path().join("settled");
    input(&mut owner, &format!("printf ok > {}", settled.display()), 3).await;
    ack(&mut owner, 3).await;
    wait_file(&settled).await;
    assert!(!marker.exists(), "revoked input reached the PTY");
    send(&mut owner, ClientMsg::OwnerRelease).await;
    loop {
        if matches!(
            recv(&mut owner).await,
            DaemonMsg::OwnerChanged {
                owner_client_id: None,
                ..
            }
        ) {
            break;
        }
    }
    owner.close(None).await.unwrap();
    if let Some(replacement) = replacement {
        // Explicitly release ownership before the replacement guest claims it.
        drop(owner);
        let mut guest = connect(address, &terminal, Some(id), &replacement).await;
        let renewed = h.root.path().join("renewed");
        input(&mut guest, &format!("printf ok > {}", renewed.display()), 4).await;
        ack(&mut guest, 4).await;
        wait_file(&renewed).await;
    }
    server.abort();
    h.stop(&terminal).await;
}
#[tokio::test]
async fn logout_refuses_new_terminal_input() {
    invalidation(false, false).await;
}
#[tokio::test]
async fn reunlock_refuses_new_terminal_input() {
    invalidation(true, false).await;
}
#[tokio::test]
async fn logout_refuses_terminal_input_already_queued_at_writer() {
    invalidation(false, true).await;
}
#[tokio::test]
async fn reunlock_refuses_terminal_input_already_queued_at_writer() {
    invalidation(true, true).await;
}

async fn idle_invalidation(mode: u8) {
    let (h, fx, terminal) = shell().await;
    let link = if mode == 4 {
        super::scoped_events::short_lived_link(&fx).await
    } else {
        fx.create(true).await
    };
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let (address, server) = super::scoped_events::serve(&fx).await;
    let mut ws = connect(address, &terminal, Some(id), &cookie).await;
    super::scoped_events::invalidate(&fx, &link, &cookie, mode).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    server.abort();
    h.stop(&terminal).await;
}
#[tokio::test]
async fn idle_terminal_closes_on_logout_without_application_event() {
    idle_invalidation(0).await;
}
#[tokio::test]
async fn idle_terminal_closes_on_reunlock_without_application_event() {
    idle_invalidation(1).await;
}
#[tokio::test]
async fn idle_terminal_closes_on_session_store_removal_without_application_event() {
    idle_invalidation(2).await;
}

#[tokio::test]
async fn idle_terminal_closes_on_link_revoke_without_application_event() {
    idle_invalidation(3).await;
}
#[tokio::test]
async fn idle_terminal_closes_on_link_expiry_without_application_event() {
    idle_invalidation(4).await;
}

#[tokio::test]
async fn paired_device_keeps_ordinary_terminal_input_contract() {
    let (h, fx, terminal) = shell().await;
    let session = fx.auth.sessions.create(
        SessionIdentity::owner(&fx.auth.config),
        SessionAuthority::PairedDevice,
    );
    let cookie = format!("{SESSION_COOKIE}={session}");
    let (address, server) = super::scoped_events::serve(&fx).await;
    let mut ws = connect(address, &terminal, None, &cookie).await;
    let marker = h.root.path().join("paired-input");
    // InputAck confirms the PTY write, not shell completion. Publish the marker
    // only after printf finishes so file existence also guarantees its contents.
    input(
        &mut ws,
        &format!(
            "printf paired > '{0}.tmp' && mv '{0}.tmp' '{0}'",
            marker.display()
        ),
        1,
    )
    .await;
    ack(&mut ws, 1).await;
    wait_file(&marker).await;
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "paired");
    ws.close(None).await.unwrap();
    server.abort();
    h.stop(&terminal).await;
}
