use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use calm_server::terminal_renderer::{
    ClientPumpContext, RendererConfig, TerminalRendererRegistry, run_client_pump,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding,
};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
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
            cols: 80,
            rows: 24,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo hello; sleep 30".into()],
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

    let (client_tx, client_rx) = mpsc::channel::<ClientMsg>(16);
    let (daemon_tx, mut daemon_rx) = mpsc::channel::<DaemonMsg>(64);
    let pump_entry = entry.clone();
    let pump = tokio::spawn(async move {
        run_client_pump(
            client_rx,
            daemon_tx,
            ClientPumpContext {
                event_rx: pump_entry.handle.event_tx.subscribe(),
                event_tx: pump_entry.handle.event_tx.clone(),
                render_plane: pump_entry.handle.render_plane.clone(),
                supervisor_tx: pump_entry.handle.supervisor_tx.clone(),
                owner_registry: pump_entry.handle.owner_registry.clone(),
                session_id: pump_entry.handle.session_id,
                terminal_id: pump_entry.terminal_id.clone(),
            },
        )
        .await
    });

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
    assert!(
        matches!(hello, DaemonMsg::ServerHello { .. }),
        "expected ServerHello, got {hello:?}"
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
