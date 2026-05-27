//! Deterministic calm-session daemon liveness probe.
//!
//! A Unix socket accepting `connect(2)` is not enough to prove that the
//! process behind it is a current calm-session daemon: stale listeners and
//! old binaries can still accept a connection. The live signal is the v2
//! protocol handshake: `ClientHello` followed by `ServerHello`.

use std::path::Path;
use std::time::Duration;

use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role, read_frame, write_frame,
};
use tokio::net::UnixStream;
use uuid::Uuid;

const TERMINAL_PROBE_BACKSTOP: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalProbe {
    Alive,
    AcceptingButStale,
    Unreachable,
}

fn normalize_terminal_id(terminal_id: &str) -> String {
    Uuid::parse_str(terminal_id)
        .map(|uuid| uuid.to_string())
        .unwrap_or_else(|_| terminal_id.to_string())
}

/// Build the minimal `ClientHello` used by non-browser kernel callers.
/// Kept shared with the sweeper's graceful Kill path so a future handshake
/// field does not drift between the two callers.
pub(crate) fn probe_client_hello(terminal_id: &str, role_hint: Option<Role>) -> ClientMsg {
    // Match the browser bridge normalization in `ws/terminal.rs`: the API
    // exposes `model::new_id()`'s simple UUID form, while `daemon.rs` passes
    // a hyphenated `cli.id` into the session context and the daemon validates
    // `ClientHello.terminal_id` with strict string equality. Non-UUID ids
    // are left unchanged so malformed ids still fail loud as `BadHandshake`.
    let terminal_id = normalize_terminal_id(terminal_id);

    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id,
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback: InitialScrollback::None,
        resume_from: None,
        role_hint,
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: false,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    }
}

/// Probe a terminal daemon by completing the calm-session handshake.
///
/// `ServerHello` with the current protocol version and normalized terminal id
/// is the only live signal. Connect failure means no process is accepting on
/// the persisted socket; `ProtocolError`, frame errors, EOF, wrong
/// `ServerHello`, and any other first frame mean a process accepted the
/// connection but is not a usable current daemon. The generous timeout is only
/// a backstop for a peer that accepts the connection and then never produces a
/// protocol signal; it is not a latency judge. Concrete transport or protocol
/// failures generally arrive fast.
pub(crate) async fn probe_terminal_daemon(sock: &Path, terminal_id: &str) -> TerminalProbe {
    let stream = match UnixStream::connect(sock).await {
        Ok(stream) => stream,
        Err(_) => return TerminalProbe::Unreachable,
    };
    match tokio::time::timeout(
        TERMINAL_PROBE_BACKSTOP,
        probe_terminal_daemon_inner(stream, terminal_id),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => TerminalProbe::AcceptingButStale,
    }
}

async fn probe_terminal_daemon_inner(stream: UnixStream, terminal_id: &str) -> TerminalProbe {
    let normalized_terminal_id = normalize_terminal_id(terminal_id);
    let (mut rd, mut wr) = stream.into_split();

    if write_frame(
        &mut wr,
        &probe_client_hello(terminal_id, Some(Role::Observer)),
    )
    .await
    .is_err()
    {
        return TerminalProbe::AcceptingButStale;
    }

    match read_frame::<DaemonMsg, _>(&mut rd).await {
        Ok(DaemonMsg::ServerHello {
            protocol_version,
            terminal_id,
            ..
        }) if protocol_version == PROTOCOL_VERSION && terminal_id == normalized_terminal_id => {
            TerminalProbe::Alive
        }
        Ok(_) | Err(_) => TerminalProbe::AcceptingButStale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_session::{ProtocolErrorCode, RenderSnapshot, Role, read_frame, write_frame};
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;

    fn server_hello_with_version(terminal_id: &str, protocol_version: u16) -> DaemonMsg {
        DaemonMsg::ServerHello {
            protocol_version,
            terminal_id: terminal_id.to_string(),
            session_id: Uuid::new_v4(),
            client_role: Role::Owner,
            owner_client_id: Some(Uuid::new_v4()),
            pty_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            pty_seq_head: 0,
            pty_seq_tail: 0,
            render_rev: 0,
            snapshot: RenderSnapshot {
                render_rev: 0,
                pty_seq: 0,
                cols: 80,
                rows: 24,
                encoding: RenderEncoding::Vt,
                data: Vec::new(),
                scrollback: None,
            },
            history_gap: None,
            is_child_ready: false,
        }
    }

    fn server_hello(terminal_id: &str) -> DaemonMsg {
        server_hello_with_version(terminal_id, PROTOCOL_VERSION)
    }

    async fn bind_once(reply: Option<DaemonMsg>) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind unix listener");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let _ = read_frame::<ClientMsg, _>(&mut stream).await;
            if let Some(reply) = reply {
                let _ = write_frame(&mut stream, &reply).await;
            }
        });
        (tmp, sock)
    }

    async fn bind_garbage_once() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind unix listener");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let _ = stream.write_all(b"not-a-calm-session-frame").await;
        });
        (tmp, sock)
    }

    #[tokio::test]
    async fn probe_server_hello_is_alive() {
        let (_tmp, sock) = bind_once(Some(server_hello("term-1"))).await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::Alive
        );
    }

    #[tokio::test]
    async fn probe_normalizes_simple_uuid_to_hyphenated_and_stays_observer() {
        let uuid = Uuid::new_v4();
        let simple_id = uuid.simple().to_string();
        let hyphenated_id = uuid.to_string();

        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("daemon.sock");
        let listener = UnixListener::bind(&sock).expect("bind unix listener");

        let expected_id = hyphenated_id.clone();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            match read_frame::<ClientMsg, _>(&mut stream)
                .await
                .expect("client hello")
            {
                ClientMsg::ClientHello {
                    terminal_id,
                    role_hint,
                    ..
                } => {
                    assert_eq!(terminal_id, expected_id);
                    assert_eq!(role_hint, Some(Role::Observer));
                }
                other => panic!("expected ClientHello, got {other:?}"),
            }
            write_frame(&mut stream, &server_hello(&expected_id))
                .await
                .expect("server hello");
        });

        assert_eq!(
            probe_terminal_daemon(&sock, &simple_id).await,
            TerminalProbe::Alive
        );
        handle.await.expect("listener task");
    }

    #[tokio::test]
    async fn probe_server_hello_with_wrong_protocol_is_accepting_but_stale() {
        let (_tmp, sock) = bind_once(Some(server_hello_with_version(
            "term-1",
            PROTOCOL_VERSION + 1,
        )))
        .await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::AcceptingButStale
        );
    }

    #[tokio::test]
    async fn probe_server_hello_with_wrong_terminal_id_is_accepting_but_stale() {
        let (_tmp, sock) = bind_once(Some(server_hello("other-term"))).await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::AcceptingButStale
        );
    }

    #[test]
    fn probe_client_hello_normalizes_uuid_and_preserves_role_hint() {
        let uuid = Uuid::new_v4();
        let simple_id = uuid.simple().to_string();
        match probe_client_hello(&simple_id, Some(Role::Observer)) {
            ClientMsg::ClientHello {
                terminal_id,
                role_hint,
                ..
            } => {
                assert_eq!(terminal_id, uuid.to_string());
                assert_eq!(role_hint, Some(Role::Observer));
            }
            other => panic!("expected ClientHello, got {other:?}"),
        }

        match probe_client_hello("term-1", None) {
            ClientMsg::ClientHello {
                terminal_id,
                role_hint,
                ..
            } => {
                assert_eq!(terminal_id, "term-1");
                assert_eq!(role_hint, None);
            }
            other => panic!("expected ClientHello, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_protocol_error_is_accepting_but_stale() {
        let (_tmp, sock) = bind_once(Some(DaemonMsg::ProtocolError {
            code: ProtocolErrorCode::BadHandshake,
            message: "nope".into(),
            expected_version: Some(PROTOCOL_VERSION),
        }))
        .await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::AcceptingButStale
        );
    }

    #[tokio::test]
    async fn probe_frame_error_is_accepting_but_stale() {
        let (_tmp, sock) = bind_garbage_once().await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::AcceptingButStale
        );
    }

    #[tokio::test]
    async fn probe_immediate_eof_is_accepting_but_stale() {
        let (_tmp, sock) = bind_once(None).await;

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::AcceptingButStale
        );
    }

    #[tokio::test]
    async fn probe_connect_refused_is_unreachable() {
        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("missing.sock");

        assert_eq!(
            probe_terminal_daemon(&sock, "term-1").await,
            TerminalProbe::Unreachable
        );
    }
}
