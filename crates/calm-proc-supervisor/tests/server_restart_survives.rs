use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::process::Command;

/// Liveness upper bound for "read the next frame / wait for the expected
/// state". Anti-hang guard only — no case here claims the supervisor reacts
/// within this budget, so a slow-but-correct run must still pass. Costs
/// nothing on the happy path (each wait returns as soon as its frame lands).
/// The 1-2s budgets this replaces are the same shape that flaked on CI's
/// 2-core runner under `retries = 0`. 120s is the `slow-timeout` of nextest
/// `profile.ci`; the local `profile.default` warns at 60s. Both are warn-only,
/// so neither kills the test — past this point nextest's slow-test report is the
/// signal, not a hand-picked deadline.
/// To assert promptness, measure elapsed and assert on it instead.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

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
///     the terminal renderer in the loop to confirm);
///   * neige-app's peer-supervision ordering (calm-proc-supervisor up
///     before calm-server) and `/admin/restart` scope.
///
/// A follow-up PR should add a `neige_app_restart_calm_server_leaves_daemon_alive`
/// E2E that walks the full stack.
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

    // #1013 — the elapsed assertion below is the load-bearing half of case 3.
    //
    // `assert!(!pid_alive(pid))` alone is near-tautological here: `src/main.rs`
    // is `#[tokio::main]`, so runtime drop waits on the in-flight
    // `spawn_blocking(move || waitpid(pid))` that `reap_children` started. The
    // supervisor process therefore *cannot* exit before the pipe child does,
    // and `supervisor.wait()` above already implies the child is gone. If
    // `pgid_lease::group_target` ever returns `Err` for a Pipe entry — which
    // drops every Pipe entry out of `terminate_all_process_groups_sync` via its
    // `filter_map(.. .ok())` and re-breaks #388 — this test does not go red, it
    // **hangs** until the fixture's own 30s self-exit releases the waitpid.
    // Measured: baseline 0.23s; with that mutation 30.03s, still "ok".
    //
    // So measure the SIGTERM→teardown latency instead. Budget rationale:
    //   * fixture self-exits at 30s (tests/fixtures/ready-sleeper/main.rs) —
    //     10s leaves a 20s margin, i.e. the mutated run trips this, not the
    //     fixture;
    //   * baseline is ~0.23s — 10s is a ~40x margin, so this is not a
    //     hand-tuned deadline and a slow CI runner will not flake it.
    // Deliberately NOT done: raising the fixture's sleep to widen the gap.
    // `try_spawn_pipe` sets `kill_on_drop(false)`, so a failing run would leak
    // a process for the whole sleep, on a box that also runs production.
    //
    // The primary gate for the Pipe-`Err` rule is the `--lib` test
    // `pgid_lease_tests::pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc`.
    // This is the secondary, end-to-end one.
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
    // The loop checks at t≈0,50,..,950ms and then sleeps a 20th time; without
    // this the last observation is at 950ms and the final sleep is wasted.
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

/// Upper bound on SIGTERM(supervisor) → pipe child dead → supervisor exited.
/// Unlike `LIVENESS_BUDGET` this one **is** an asserted claim about promptness;
/// see the rationale at its use site.
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
