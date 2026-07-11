use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use calm_server::terminal_renderer::{
    ClientPumpContext, RendererConfig, RendererEntry, TerminalRendererRegistry, run_client_pump,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding,
};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test]
async fn in_process_renderer_drives_real_supervisor_and_pty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let registry = TerminalRendererRegistry::new();
    let terminal_id = Uuid::new_v4().to_string();
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 100,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "echo hello; printf '%085dWIDTH-MARKER\\n' 0; sleep 30".into(),
            ],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_hello_patch(&mut events).await;
    wait_for_child_ready(&mut events).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry.clone());

    client_tx
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal_id.clone(),
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
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .expect("send client hello");
    let hello = timeout(Duration::from_secs(2), daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    let DaemonMsg::ServerHello { snapshot, .. } = hello else {
        panic!("expected ServerHello");
    };
    assert_eq!((snapshot.cols, snapshot.rows), (100, 24));
    assert!(
        String::from_utf8_lossy(&snapshot.data).contains("WIDTH-MARKER"),
        "80-col remount desire must not clip marker from current 100-col model"
    );

    client_tx
        .send(ClientMsg::Input {
            data: b"\x03".to_vec(),
            input_seq: 1,
        })
        .await
        .expect("send ctrl-c");
    wait_for_input_ack(&mut daemon_rx, 1).await;
    wait_for_terminal_exited(&mut events).await;
    wait_for_daemon_terminal_exited(&mut daemon_rx).await;
    assert!(
        timeout(Duration::from_secs(2), entry.wait_exited())
            .await
            .expect("entry exit timeout")
            .is_some(),
        "renderer entry did not surface supervisor Exited"
    );

    registry.drop_entry(&terminal_id).await;
    assert!(registry.get(&terminal_id).is_none());

    drop(client_tx);
    timeout(Duration::from_secs(2), pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

#[tokio::test]
async fn late_client_attach_receives_sticky_terminal_exited() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let registry = TerminalRendererRegistry::new();
    let terminal_id = Uuid::new_v4().to_string();
    let entry = registry
        .ensure(RendererConfig {
            terminal_id: terminal_id.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo late; exit 7".into()],
            envs: std::env::vars().collect(),
            cwd: workspace_root().display().to_string(),
            supervisor_sock: control_sock.clone(),
        })
        .await
        .expect("ensure renderer");

    let mut events = entry
        .take_initial_event_rx()
        .expect("initial renderer event receiver");
    wait_for_terminal_exited(&mut events).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry.clone());
    send_client_hello(&client_tx, &terminal_id).await;
    let hello = timeout(Duration::from_secs(2), daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    assert!(
        matches!(hello, DaemonMsg::ServerHello { .. }),
        "expected ServerHello, got {hello:?}"
    );
    let exit = wait_for_daemon_terminal_exited(&mut daemon_rx).await;
    assert_eq!(exit.code, Some(7));

    registry.drop_entry(&terminal_id).await;
    drop(client_tx);
    timeout(Duration::from_secs(2), pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

#[tokio::test]
async fn registry_ensure_lazily_reattaches_when_registry_is_empty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&control_sock).await;

    let terminal_id = Uuid::new_v4().to_string();
    let cfg = RendererConfig {
        terminal_id: terminal_id.clone(),
        cols: 80,
        rows: 24,
        buffer_bytes: 1 << 20,
        terminal_fg: (216, 219, 226),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "cat".into()],
        envs: std::env::vars().collect(),
        cwd: workspace_root().display().to_string(),
        supervisor_sock: control_sock.clone(),
    };

    let registry_a = TerminalRendererRegistry::new();
    let entry_a = registry_a
        .ensure(cfg.clone())
        .await
        .expect("ensure initial renderer");
    assert!(
        registry_a.get(&terminal_id).is_some(),
        "initial registry should contain renderer entry"
    );
    drop(entry_a);
    drop(registry_a);

    let registry_b = TerminalRendererRegistry::new();
    let entry_b = registry_b
        .ensure(cfg)
        .await
        .expect("reattach renderer after registry drop");
    assert!(
        registry_b.get(&terminal_id).is_some(),
        "fresh registry should contain reattached renderer entry"
    );

    let mut events = entry_b.subscribe();
    assert_no_terminal_exited(&mut events, Duration::from_secs(1)).await;

    let (client_tx, mut daemon_rx, pump) = spawn_client_pump(entry_b.clone());
    send_client_hello(&client_tx, &terminal_id).await;
    let hello = timeout(Duration::from_secs(2), daemon_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    assert!(
        matches!(hello, DaemonMsg::ServerHello { .. }),
        "expected ServerHello, got {hello:?}"
    );

    client_tx
        .send(ClientMsg::Input {
            data: b"lazy-reattach\n".to_vec(),
            input_seq: 7,
        })
        .await
        .expect("send input through reattached renderer");
    wait_for_input_ack(&mut daemon_rx, 7).await;

    registry_b.drop_entry(&terminal_id).await;
    drop(client_tx);
    timeout(Duration::from_secs(2), pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
}

fn spawn_client_pump(
    entry: Arc<RendererEntry>,
) -> (
    mpsc::Sender<ClientMsg>,
    mpsc::Receiver<DaemonMsg>,
    JoinHandle<anyhow::Result<()>>,
) {
    let (client_tx, client_rx) = mpsc::channel::<ClientMsg>(16);
    let (daemon_tx, daemon_rx) = mpsc::channel::<DaemonMsg>(64);
    let pump = tokio::spawn(async move {
        run_client_pump(
            client_rx,
            daemon_tx,
            ClientPumpContext {
                event_rx: entry.handle.event_tx.subscribe(),
                event_tx: entry.handle.event_tx.clone(),
                render_plane: entry.handle.render_plane.clone(),
                exit: entry.exit.clone(),
                supervisor_tx: entry.handle.supervisor_tx.clone(),
                owner_registry: entry.handle.owner_registry.clone(),
                session_id: entry.handle.session_id,
                terminal_id: entry.terminal_id.clone(),
            },
        )
        .await
    });
    (client_tx, daemon_rx, pump)
}

async fn send_client_hello(client_tx: &mpsc::Sender<ClientMsg>, terminal_id: &str) {
    client_tx
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal_id.to_string(),
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
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .expect("send client hello");
}

async fn wait_for_hello_patch(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for hello patch");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("hello patch timeout")
            .expect("event channel closed");
        if let DaemonMsg::RenderPatch(patch) = msg
            && String::from_utf8_lossy(&patch.data).contains("hello")
        {
            return;
        }
    }
}

async fn wait_for_child_ready(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for ChildReady");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("child ready timeout")
            .expect("event channel closed");
        if matches!(msg, DaemonMsg::ChildReady { .. }) {
            return;
        }
    }
}

async fn wait_for_input_ack(rx: &mut mpsc::Receiver<DaemonMsg>, expected: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for InputAck");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("input ack timeout")
            .expect("daemon channel closed");
        if let DaemonMsg::InputAck { input_seq } = msg {
            assert_eq!(input_seq, expected);
            return;
        }
    }
}

async fn wait_for_terminal_exited(rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for TerminalExited");
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("terminal exited timeout")
            .expect("event channel closed");
        if matches!(msg, DaemonMsg::TerminalExited { .. }) {
            return;
        }
    }
}

async fn assert_no_terminal_exited(
    rx: &mut tokio::sync::broadcast::Receiver<DaemonMsg>,
    duration: Duration,
) {
    let deadline = tokio::time::Instant::now() + duration;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(DaemonMsg::TerminalExited { .. })) => {
                panic!("reattached live renderer unexpectedly emitted TerminalExited");
            }
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("reattached renderer event channel closed");
            }
            Err(_) => return,
        }
    }
}

struct ExitFrame {
    code: Option<i32>,
}

async fn wait_for_daemon_terminal_exited(rx: &mut mpsc::Receiver<DaemonMsg>) -> ExitFrame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for daemon TerminalExited"
        );
        let msg = timeout(remaining, rx.recv())
            .await
            .expect("daemon terminal exited timeout")
            .expect("daemon channel closed");
        if let DaemonMsg::TerminalExited { code, .. } = msg {
            return ExitFrame { code };
        }
    }
}

async fn spawn_proc_supervisor(control_sock: &Path) -> Child {
    let child = Command::new(locate_bin("calm-proc-supervisor"))
        .arg("--control-sock")
        .arg(control_sock)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn supervisor");
    wait_until_listening(control_sock).await;
    assert!(child.id().is_some(), "supervisor exited before listening");
    child
}

async fn wait_until_listening(sock: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if UnixStream::connect(sock).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("supervisor never listened on {}", sock.display());
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "calm-proc-supervisor",
            "--bin",
            "calm-proc-supervisor",
            "--locked",
        ])
        .status()
        .expect("run cargo build for calm-proc-supervisor");
    assert!(
        status.success(),
        "cargo build for calm-proc-supervisor failed with {status}"
    );
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}
