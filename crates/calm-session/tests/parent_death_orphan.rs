//! #267 E2E — `calm-session-daemon` MUST die when its parent dies, and
//! the PTY child it was supervising MUST die with it.
//!
//! The incident this guards against: 250+ orphan `calm-session-daemon`
//! processes (reparented to `systemd --user`, PPID 1) holding 175+
//! orphan `codex` CLIs alive over a ~4.5h test-runner background
//! window. Cause: no `prctl(PR_SET_PDEATHSIG)` on Linux + no
//! graceful-shutdown path that kills the codex child the daemon
//! owns. The fix in `bin/daemon.rs::install_parent_death_watcher` is
//! kernel-level on Linux and a `getppid()` polling task on other
//! unix-like targets; this test reproduces the orphan scenario and
//! asserts the fix holds.
//!
//! Linux-gated because the kernel hook (`PR_SET_PDEATHSIG`) is
//! Linux-only and the test inspects `/proc/<pid>` for liveness. The
//! non-Linux fallback (a `getppid()` poller in
//! `install_parent_death_watcher`) takes the same shutdown path
//! (`SIGTERM` → `kill_child`), so a Linux-side green test covers the
//! shared shutdown code; only the trigger differs by platform.

#![cfg(target_os = "linux")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, read_frame, write_frame,
};
use uuid::Uuid;

fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_calm-session-daemon")
}

/// Truthy if `/proc/<pid>` exists. Cheap, no syscall, doesn't depend
/// on the test process having permission to signal the target — which
/// matters here because the orphaned daemon's effective owner is still
/// us, but we want to keep the check pure-read so a future test that
/// runs as a different uid still works.
fn pid_alive(pid: i32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

/// Block until `pid` disappears from `/proc` or the deadline passes.
/// Returns `true` on disappear, `false` on timeout.
fn wait_for_exit(pid: i32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // #272 (N4) — the loop already checked `!pid_alive` on every
    // iteration up to the deadline; a final re-check here is dead
    // weight. Timeout means we waited the full budget without seeing
    // the pid disappear, which is the definition of "not exited".
    false
}

fn fresh_sock(label: &str) -> PathBuf {
    let id = Uuid::new_v4();
    let p = std::env::temp_dir().join(format!("calm-orphan-{label}-{id}.sock"));
    let _ = std::fs::remove_file(&p);
    p
}

/// Parse `pgrep -P <pid>` output into a Vec<i32>. Returns empty if pgrep
/// finds no children (exit code 1) or fails.
fn children_of(pid: i32) -> Vec<i32> {
    let out = match Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .collect()
}

/// Recursive descendant collection: walks `pgrep -P` from `pid` and
/// returns every reachable descendant pid. Used so we can SIGKILL the
/// intermediate `sh` and then ask "did the daemon (a grandchild) and
/// its `sleep` (a great-grandchild) both go away?".
fn all_descendants(pid: i32) -> Vec<i32> {
    let mut out = Vec::new();
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        for c in children_of(p) {
            out.push(c);
            stack.push(c);
        }
    }
    out
}

/// Driver: spawn `sh -c "<daemon> & echo $!; wait"` and return
/// `(sh_pid, daemon_pid, sock_path)`. The `wait` keeps sh alive until
/// the daemon exits so we can choose the kill moment. `echo $!` makes
/// the daemon pid recoverable from sh's stdout. `sleep 600` is the
/// stand-in PTY child — long enough that the test will never hit its
/// natural exit.
struct Driver {
    sh: std::process::Child,
    daemon_pid: i32,
    sock: PathBuf,
    _supervisor: common::SupervisorHandle,
}

fn launch_daemon_under_sh(label: &str) -> Driver {
    let sock = fresh_sock(label);
    let id = Uuid::new_v4().to_string();
    let supervisor = common::spawn_proc_supervisor();
    let cmd = format!(
        "{daemon} --id {id} --sock {sock} \
            --proc-supervisor-sock {supervisor_sock} \
            --terminal-fg 216,219,226 --terminal-bg 15,20,24 \
            -- sh -c 'sleep 600' & echo $!; wait",
        daemon = daemon_bin(),
        id = id,
        sock = sock.to_string_lossy(),
        supervisor_sock = supervisor.sock.to_string_lossy(),
    );
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn sh");

    // Read the first stdout line — `echo $!` printed the daemon's pid
    // before `wait` blocks. Doing this inline (not in a thread) is
    // fine: `sh -c '<cmd> & echo $!'` flushes after echo because the
    // pipe buffer fits a single integer.
    use std::io::{BufRead, BufReader};
    let stdout = sh.stdout.take().expect("sh stdout");
    let mut rdr = BufReader::new(stdout);
    let mut line = String::new();
    rdr.read_line(&mut line).expect("read daemon pid");
    let daemon_pid: i32 = line.trim().parse().expect("parse daemon pid");

    // Wait up to 3s for the daemon to bind its socket — same budget as
    // `daemon_cli_theme.rs::wait_bind_or_exit`. Past this and either
    // the daemon is broken or we picked up the wrong pid from echo.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        sock.exists(),
        "daemon (pid {daemon_pid}) failed to bind socket {} within 3s",
        sock.display()
    );

    Driver {
        sh,
        daemon_pid,
        sock,
        _supervisor: supervisor,
    }
}

#[test]
fn parent_death_kills_daemon_and_pty_child() {
    let mut d = launch_daemon_under_sh("kill-parent");
    let daemon_pid = d.daemon_pid;
    // Phase 2 (#388): the PTY child is forked by `calm-proc-supervisor`,
    // not the daemon. Enumerate the supervisor's descendants so we can
    // assert they die after parent-death. The daemon's parent-death
    // branch sends Signal{Term} (+ post-grace Signal{Kill}) to the
    // supervisor over a fresh UDS connection, the supervisor SIGTERMs
    // the PTY pgid, and the child should be reaped before our budget.
    let supervisor_pid = d._supervisor.child.id() as i32;
    let pty_children = all_descendants(supervisor_pid);
    assert!(
        !pty_children.is_empty(),
        "expected proc-supervisor (pid {supervisor_pid}) to have at least one PTY descendant; \
         daemon pid {daemon_pid} delegates PTY ownership to supervisor in Phase 2",
    );

    // SIGKILL the wrapping sh. The daemon (its child) is now an orphan
    // — without `PR_SET_PDEATHSIG` it would survive indefinitely.
    let sh_pid = d.sh.id() as i32;
    unsafe {
        libc::kill(sh_pid, libc::SIGKILL);
    }
    let _ = d.sh.wait();

    // Daemon should die within ~1s: the kernel delivers SIGTERM on
    // reparent (PR_SET_PDEATHSIG), the daemon's tokio signal handler
    // catches it, kill_child fires SIGHUP at the pgid (with a 2s
    // SIGKILL fallback), then the child-waiter sees exit and the
    // daemon shuts down. 5s budget gives 3x headroom for slow CI.
    assert!(
        wait_for_exit(daemon_pid, Duration::from_secs(5)),
        "daemon (pid {daemon_pid}) survived parent death — PR_SET_PDEATHSIG / shutdown handler regression",
    );

    // PTY child must also be gone — that's the whole point. The daemon
    // SIGHUPs its child's process group on shutdown; we give a slightly
    // longer budget here to absorb the daemon's 2s SIGKILL fallback +
    // the kernel's reaping window.
    for pty in &pty_children {
        assert!(
            wait_for_exit(*pty, Duration::from_secs(5)),
            "PTY descendant pid {pty} of daemon {daemon_pid} survived parent death — codex would leak",
        );
    }

    // Socket cleanup is best-effort in the daemon's shutdown path; we
    // don't assert on it (the daemon may not reach the `remove_file`
    // call if SIGKILL fallback fires first). Belt-and-braces local
    // cleanup keeps the temp dir tidy for the next run.
    let _ = std::fs::remove_file(&d.sock);
}

/// #272 (N1) — chat-mode counterpart to the terminal-mode test above.
/// Spawns a chat-mode daemon under sh, with an "unresponsive" Node
/// stub runner that ignores stdin EOF + SIGHUP + SIGPIPE; SIGKILLs
/// the parent sh; asserts that the runner's pid (which is also its
/// pgid because the daemon spawns the runner with `process_group(0)`)
/// disappears within the same 5 s budget the terminal-mode test
/// uses.
///
/// Pre-#272 the daemon's SIGTERM branch in `run_chat` only dropped
/// `stdin_tx` to deliver EOF to the runner — a fine happy path, but a
/// hung runner that traps SIGPIPE / ignores EOF would survive
/// indefinitely (only the OS reaper at daemon exit would catch it,
/// and only on the next kernel reaping pass). The unresponsive stub
/// reproduces that exact failure mode; the test passes only if the
/// new `kill_chat_runner_group` SIGHUP + 2 s SIGKILL fallback fires.
///
/// Also exercises R3 incidentally: if the tokio SIGTERM handler isn't
/// registered before the prctl race-guard self-SIGTERM (the case
/// pre-#272 R3), the daemon dies on default disposition immediately
/// at startup and the test fails with "daemon never bound socket".
#[test]
fn parent_death_kills_chat_runner_group() {
    if std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skipping parent_death_kills_chat_runner_group: node not on PATH");
        return;
    }
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("unresponsive-stub-runner.mjs");
    assert!(stub.exists(), "stub missing at {}", stub.display());

    let sock = fresh_sock("chat-kill");
    let id = Uuid::new_v4().to_string();
    let cmd = format!(
        "{daemon} --mode chat --id {id} --sock {sock} \
            --runner-path {stub} --cwd /tmp & echo $!; wait",
        daemon = daemon_bin(),
        id = id,
        sock = sock.to_string_lossy(),
        stub = stub.to_string_lossy(),
    );
    let mut sh = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn sh");
    use std::io::{BufRead, BufReader};
    let stdout = sh.stdout.take().expect("sh stdout");
    let mut rdr = BufReader::new(stdout);
    let mut line = String::new();
    rdr.read_line(&mut line).expect("read daemon pid");
    let daemon_pid: i32 = line.trim().parse().expect("parse daemon pid");

    // Wait for the socket to bind. If R3 regressed (handler installed
    // after prctl race-guard), the race-guard self-SIGTERM would kill
    // the daemon before this point and we'd never see a bound socket.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        sock.exists(),
        "chat daemon (pid {daemon_pid}) failed to bind socket {} within 3s — possibly R3 regression",
        sock.display()
    );

    // Capture the runner (node) pid via pgrep -P <daemon>. The daemon
    // spawns one node child; pgrep returns just that pid. We grab it
    // BEFORE killing sh because once the daemon is orphaned, pgrep
    // walks the reparent and the relationship to descendants would
    // need a different traversal.
    let descendants = all_descendants(daemon_pid);
    assert!(
        !descendants.is_empty(),
        "expected chat daemon (pid {daemon_pid}) to have spawned a node runner; found no descendants",
    );

    // SIGKILL the wrapping sh — orphans the daemon, kernel delivers
    // SIGTERM via PR_SET_PDEATHSIG, daemon's R3-armed handler catches
    // it, drops runner stdin (happy path no-op here because the stub
    // ignores EOF), then fires `kill_chat_runner_group` (SIGHUP + 2 s
    // SIGKILL fallback). The runner ignores SIGHUP — only the SIGKILL
    // can take it down.
    let sh_pid = sh.id() as i32;
    unsafe {
        libc::kill(sh_pid, libc::SIGKILL);
    }
    let _ = sh.wait();

    assert!(
        wait_for_exit(daemon_pid, Duration::from_secs(5)),
        "chat daemon (pid {daemon_pid}) survived parent death — R3 / shutdown handler regression",
    );
    // 5 s budget covers: ~ms PR_SET_PDEATHSIG delivery, ~ms SIGHUP
    // (ignored by stub), 2 s SIGKILL fallback timer, ~ms kernel
    // reaping. Same headroom shape as the terminal-mode test.
    for pid in &descendants {
        assert!(
            wait_for_exit(*pid, Duration::from_secs(5)),
            "chat runner descendant pid {pid} survived parent death — N1 / kill_chat_runner_group regression",
        );
    }

    let _ = std::fs::remove_file(&sock);
}

#[test]
fn parent_death_self_terminates_via_sigterm_path() {
    // Confirm the shutdown path is SIGTERM-driven (the signal
    // PR_SET_PDEATHSIG sends), not a side-effect of the broken pipe on
    // stdin/stdout/stderr. We deliver SIGTERM directly to the daemon
    // and assert it exits — same shutdown handler the orphan path
    // takes. This isolates "daemon honors SIGTERM" from "daemon
    // notices its parent is gone" so a regression in the latter
    // doesn't masquerade as a regression in the former.
    let mut d = launch_daemon_under_sh("sigterm-direct");
    let daemon_pid = d.daemon_pid;
    unsafe {
        libc::kill(daemon_pid, libc::SIGTERM);
    }
    assert!(
        wait_for_exit(daemon_pid, Duration::from_secs(5)),
        "daemon (pid {daemon_pid}) ignored SIGTERM — shutdown handler regression",
    );

    // sh wraps `wait`; the daemon exiting should let it return.
    let _ = d.sh.wait();
    let _ = std::fs::remove_file(&d.sock);
}

async fn probe_supervisor_running(sock: &std::path::Path, proc_id: &str) -> bool {
    let mut conn = tokio::net::UnixStream::connect(sock)
        .await
        .expect("connect proc supervisor for probe");
    write_frame(
        &mut conn,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: proc_id.to_string(),
        }),
    )
    .await
    .expect("write probe");
    match tokio::time::timeout(
        Duration::from_secs(1),
        read_frame::<ControlReply, _>(&mut conn),
    )
    .await
    .expect("probe reply timeout")
    .expect("probe reply decode")
    {
        ControlReply::ProbeOk { proc_running, .. } => proc_running,
        other => panic!("expected ProbeOk, got {other:?}"),
    }
}

#[tokio::test]
async fn parent_death_signal_bypasses_blocked_supervisor_writer() {
    let supervisor = common::spawn_proc_supervisor();
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let proc_id = format!("term:{id}");
    let sock = std::env::temp_dir().join(format!("calm-orphan-blocked-writer-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let mut daemon = tokio::process::Command::new(daemon_bin)
        .args(["--mode", "terminal"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .arg("--proc-supervisor-sock")
        .arg(&supervisor.sock)
        .args(["--terminal-fg", "216,219,226"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--cwd", "/tmp"])
        .args(["--", "sh", "-c", "sleep 60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    let mut bound = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::UnixStream::connect(&sock).await.is_ok() {
            bound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(bound, "daemon did not bind socket within 6s");

    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .expect("client connect");
    let (mut rd, mut wr) = stream.into_split();
    write_frame(
        &mut wr,
        &ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: id.to_string(),
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
        },
    )
    .await
    .expect("write client hello");
    let _: DaemonMsg = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut rd))
        .await
        .expect("server hello timeout")
        .expect("server hello decode");

    // Fill the PTY input queue behind a child that never reads stdin.
    // This leaves the queued supervisor writer stuck in WriteStdin, so
    // the parent-death Signal must use a fresh supervisor connection.
    let bulk = vec![b'a'; 8 * 1024 * 1024];
    tokio::time::timeout(
        Duration::from_secs(3),
        write_frame(
            &mut wr,
            &ClientMsg::Input {
                data: bulk,
                input_seq: 1,
            },
        ),
    )
    .await
    .expect("input frame write timeout")
    .expect("write input frame");
    tokio::time::sleep(Duration::from_millis(250)).await;

    let daemon_pid = daemon.id().expect("daemon pid") as libc::pid_t;
    unsafe {
        libc::kill(daemon_pid, libc::SIGTERM);
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), daemon.wait())
        .await
        .expect("daemon wait timeout")
        .expect("wait daemon");

    let probe_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < probe_deadline {
        if !probe_supervisor_running(&supervisor.sock, &proc_id).await {
            let _ = std::fs::remove_file(&sock);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("supervisor still reports {proc_id} running after daemon SIGTERM");
}
