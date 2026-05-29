use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::process::Command;

/// What this test proves and what it doesn't:
///
/// **Proves** — the proc-supervisor's load-bearing OS-level invariants:
///   1. After a client (i.e. simulated calm-server) drops the UDS
///      connection, the daemon process the supervisor spawned remains
///      alive. This is the OS-level shape of "calm-server restart leaves
///      daemons alive": from the supervisor's POV the restart looks
///      exactly like a connection close followed by a fresh connection.
///   2. EnsureProc is idempotent on `proc_id` — a reconnecting client
///      gets the existing pid back, not a duplicate fork. This is the
///      original primitive that boot-time reattach in calm-server will
///      rely on.
///   3. When the supervisor itself receives SIGTERM it tears down every
///      live proc (the explicit "supervisor death drops procs" Non-goal
///      from #388).
///
/// **Does NOT prove** — these belong to a fuller stack-level test that
/// would spawn an actual `neige-app` + `calm-server`:
///   * the daemon's `PR_SET_PDEATHSIG` is correctly anchored to the
///     supervisor and not calm-server (it is, by construction — the
///     supervisor is now the spawn-parent — but this test doesn't put
///     a real `calm-session-daemon` in the loop to confirm);
///   * neige-app's peer-supervision ordering (calm-proc-supervisor up
///     before calm-server) and `/admin/restart` scope.
///
/// A follow-up PR should add a `neige_app_restart_calm_server_leaves_daemon_alive`
/// E2E that walks the full stack.
#[tokio::test]
async fn proc_outlives_client_disconnect_and_dies_with_supervisor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let control_sock = temp.path().join("proc-supervisor.sock");
    let mut supervisor = Command::new(locate_bin("calm-proc-supervisor"))
        .arg("--control-sock")
        .arg(&control_sock)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn supervisor");
    wait_until_listening(&control_sock).await;

    let request = ensure_request(&temp);
    let pid = ensure(&control_sock, request.clone()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        pid_alive(pid),
        "child should survive dropped control connection"
    );

    let same_pid = ensure(&control_sock, request).await;
    assert_eq!(same_pid, pid, "EnsureProc must be idempotent by proc_id");

    unsafe {
        libc::kill(
            supervisor.id().expect("supervisor pid") as libc::pid_t,
            libc::SIGTERM,
        );
    }
    let _ = supervisor.wait().await.expect("wait supervisor");
    for _ in 0..20 {
        if !pid_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!pid_alive(pid), "child should die when supervisor exits");
}

async fn ensure(control_sock: &Path, request: EnsureProcRequest) -> u32 {
    let mut stream = UnixStream::connect(control_sock)
        .await
        .expect("connect supervisor");
    write_frame(&mut stream, &ControlMsg::EnsureProc(request))
        .await
        .expect("write ensure");
    let pid = match read_frame(&mut stream).await.expect("read first reply") {
        ControlReply::Spawned { pid } => pid,
        ControlReply::SpawnFailed { error, .. } => panic!("spawn failed: {error}"),
        other => panic!("unexpected first reply: {other:?}"),
    };
    match read_frame(&mut stream).await.expect("read second reply") {
        ControlReply::Ready => pid,
        ControlReply::ReadyFailed { error, .. } => panic!("ready failed: {error}"),
        other => panic!("unexpected second reply: {other:?}"),
    }
}

fn ensure_request(temp: &TempDir) -> EnsureProcRequest {
    EnsureProcRequest {
        proc_id: "terminal-1".into(),
        program: locate_bin("proc-supervisor-ready-sleeper")
            .display()
            .to_string(),
        args: vec![
            "--id".into(),
            "terminal-1".into(),
            "--sock".into(),
            temp.path().join("session.sock").display().to_string(),
            "--ready-fd".into(),
            "0".into(),
        ],
        envs: Vec::new(),
        cwd: temp.path().display().to_string(),
        ready_timeout_ms: 2_000,
        io_mode: IoMode::Pipe,
        replay_bytes: 0,
    }
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

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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
    panic!("{name} binary not found at {}", candidate.display());
}
