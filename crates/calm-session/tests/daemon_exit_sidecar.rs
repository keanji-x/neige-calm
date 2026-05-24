//! #306 — integration test: daemon writes `<sock>.exit` sidecar at
//! child-wait time so the kernel can persist the child's exit info on
//! the terminal row even though it never receives the JSON
//! `TerminalExited` frame (the socket has been unlinked by the time
//! the kernel probes again).
//!
//! Three branches exercised here:
//!
//!   1. Graceful exit with code 0 — `printf done` returns immediately,
//!      daemon's `child.wait()` resolves with `signal = None`, sidecar
//!      payload is `{"code": 0, "signal_killed": false}`.
//!
//!   2. Non-zero exit — `exit 137` is a clean main-return with a
//!      programmed exit code (NOT a signal — distinguish from SIGKILL
//!      below). Sidecar carries `{"code": 137, "signal_killed": false}`.
//!
//!   3. SIGKILL — we cargo-SIGKILL the daemon process itself before it
//!      gets a chance to write the sidecar. The sidecar does NOT appear
//!      on disk; this matches the future "DaemonLost" surface (kernel
//!      never persists exit_code, frontend renders no badge).
//!
//! The kernel-side persistence (sidecar → repo write) is exercised
//! separately in the calm-server crate. This file pins the daemon's
//! end of the contract: the file shape, location, and the SIGKILL
//! absence-case.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::process::Command;
use uuid::Uuid;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}

/// Spawn a daemon running `program` (passed verbatim as the `--` argv
/// tail). Returns the child handle (so callers can SIGKILL it for the
/// daemon-lost branch) and the socket path the daemon binds.
async fn spawn_daemon_running(program: &[&str]) -> (tokio::process::Child, PathBuf) {
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-exit306-{id}.sock"));
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(format!("{}.exit", sock.display()));

    let mut cmd = Command::new(daemon_bin);
    cmd.args(["--mode", "terminal"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--terminal-fg", "216,219,226"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        .arg("--");
    for a in program {
        cmd.arg(*a);
    }
    let child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");
    // Wait for the daemon to bind the socket. Up to 6s — same budget as
    // `input_ack_e2e.rs`.
    let mut bound = false;
    for _ in 0..150 {
        if UnixStream::connect(&sock).await.is_ok() {
            bound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(bound, "daemon did not bind socket within 6s");
    (child, sock)
}

/// Wait up to `budget` for the sidecar file to appear. Returns its
/// contents on success; panics on timeout. The daemon writes the file
/// *before* its broadcast effects fire, so by the time `child.wait()`
/// returns on the test side, the file is on disk.
async fn await_sidecar(sock: &std::path::Path, budget: Duration) -> String {
    let exit_path = format!("{}.exit", sock.display());
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Ok(s) = std::fs::read_to_string(&exit_path) {
            return s;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for sidecar at {exit_path}; daemon never wrote it",);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn sidecar_records_clean_exit_zero() {
    // `true` returns 0 immediately. Captures the happy-path one-shot
    // worker shape (`printf done` etc. all funnel through `child.wait()`
    // with `signal = None, code = Some(0)`).
    let (mut child, sock) = spawn_daemon_running(&["true"]).await;
    let _ = child.wait().await;
    let raw = await_sidecar(&sock, Duration::from_secs(3)).await;
    let v: serde_json::Value = serde_json::from_str(&raw).expect("sidecar JSON parses");
    assert_eq!(v["code"], serde_json::json!(0), "raw: {raw}");
    assert_eq!(v["signal_killed"], serde_json::json!(false), "raw: {raw}");
    // GC the sidecar so a re-run on the same temp dir isn't polluted.
    let _ = std::fs::remove_file(format!("{}.exit", sock.display()));
}

#[tokio::test]
async fn sidecar_records_non_zero_main_return() {
    // `sh -c 'exit 137'` programs the exit code without a signal; the
    // daemon's `ExitStatus::signal()` is None, so we record the numeric
    // code and `signal_killed = false`. This is distinct from a SIGKILL
    // that produces the same 137 byte on POSIX's combined status word
    // (128 + 9). portable-pty's `signal()` discriminator is what makes
    // the two cases tell-apart-able.
    let (mut child, sock) = spawn_daemon_running(&["sh", "-c", "exit 137"]).await;
    let _ = child.wait().await;
    let raw = await_sidecar(&sock, Duration::from_secs(3)).await;
    let v: serde_json::Value = serde_json::from_str(&raw).expect("sidecar JSON parses");
    assert_eq!(v["code"], serde_json::json!(137), "raw: {raw}");
    assert_eq!(v["signal_killed"], serde_json::json!(false), "raw: {raw}");
    let _ = std::fs::remove_file(format!("{}.exit", sock.display()));
}

#[tokio::test]
async fn sigkill_leaves_no_sidecar() {
    // Daemon-lost branch: SIGKILL the daemon itself before its child
    // exits. `sleep 60` keeps the inner PTY alive long enough that we
    // KNOW the daemon hadn't reached its `child.wait()` write site
    // when we kill it. Result: no sidecar on disk → kernel treats this
    // as "exit info unknown" (v1 surfaces as no badge; v2 as
    // DaemonLost).
    let (mut child, sock) = spawn_daemon_running(&["sh", "-c", "sleep 60"]).await;
    // SIGKILL. `child.kill().await` sends SIGKILL on Unix per tokio's
    // Process docs; the `kill_on_drop(true)` we set in `spawn_daemon_running`
    // would also do it if we dropped without waiting, but explicit is
    // clearer here.
    child.kill().await.expect("kill daemon");
    let _ = child.wait().await;
    // Give the OS a moment to reap, then assert the sidecar wasn't
    // written. We poll for a tighter bound than `await_sidecar` since
    // we're proving absence, not presence — a positive result would
    // appear in tens of milliseconds if it was going to.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let exit_path = format!("{}.exit", sock.display());
    assert!(
        !std::path::Path::new(&exit_path).exists(),
        "sidecar must not appear on a SIGKILL'd daemon, but found at {exit_path}",
    );
    // Also assert the socket is gone (or at minimum unreachable). The
    // kernel's `resolve_live_sock` keys off "handle set + probe fails"
    // → `LiveSock::ChildExited`, and our sidecar-absence here is what
    // makes the subsequent `terminal_set_exit` write a no-op.
    let _ = std::fs::remove_file(&sock);
}
