use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::process::Command;

/// Anti-hang guard only; no case here claims the supervisor reacts within this budget, so a slow-but-correct run must still pass.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

/// A dropped UDS connection leaves the spawned daemon alive, EnsureProc is idempotent on `proc_id`, and SIGTERM to the supervisor tears down every live proc.
#[tokio::test]
async fn proc_outlives_client_disconnect_and_dies_with_supervisor() {
    let temp = calm_test_sockets::socket_dir("ps");
    let control_sock = calm_test_sockets::socket_path(temp.path(), "proc-supervisor.sock");
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

    // The elapsed assertion is the load-bearing half: `src/main.rs` is `#[tokio::main]`, so runtime drop blocks on the `reap_children` waitpid and the supervisor cannot exit before the pipe child does.
    // If shutdown stopped group-SIGTERMing Pipe entries this test would hang until the fixture's 30s self-exit, not go red. 10s leaves a 20s margin against that and ~40x against the ~0.23s baseline.
    let t0 = std::time::Instant::now();
    unsafe {
        libc::kill(
            supervisor.id().expect("supervisor pid") as libc::pid_t,
            libc::SIGTERM,
        );
    }
    let _ = supervisor.wait().await.expect("wait supervisor");
    let mut died = false;
    for _ in 0..20 {
        if !pid_alive(pid) {
            died = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // One more observation after the 20th sleep, so the final sleep is not wasted.
    let died = died || !pid_alive(pid);
    let elapsed = t0.elapsed();
    assert!(died, "child should die when supervisor exits");
    assert!(
        elapsed < TEARDOWN_BUDGET,
        "supervisor took {elapsed:?} (> {TEARDOWN_BUDGET:?}) to SIGTERM its pipe procs and exit. \
         The child did eventually die, but not because the supervisor killed its process group — \
         most likely it self-exited at the fixture's 30s mark while the supervisor's \
         `#[tokio::main]` runtime drop blocked on the reap_children waitpid. Check that \
         `pgid_lease::group_target` still returns `Ok(PipeBestEffort)` for Pipe entries \
         (regulation 4) so `terminate_all_process_groups_sync` does not filter them out."
    );
}

/// Upper bound on SIGTERM(supervisor) → pipe child dead → supervisor exited. Unlike `LIVENESS_BUDGET` this one is an asserted claim about promptness.
const TEARDOWN_BUDGET: Duration = Duration::from_secs(10);

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
            calm_test_sockets::socket_path(temp.path(), "session.sock")
                .display()
                .to_string(),
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
    let deadline = tokio::time::Instant::now() + LIVENESS_BUDGET;
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
