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

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
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
    !pid_alive(pid)
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
}

fn launch_daemon_under_sh(label: &str) -> Driver {
    let sock = fresh_sock(label);
    let id = Uuid::new_v4().to_string();
    let cmd = format!(
        "{daemon} --id {id} --sock {sock} \
            --terminal-fg 216,219,226 --terminal-bg 15,20,24 \
            -- sh -c 'sleep 600' & echo $!; wait",
        daemon = daemon_bin(),
        id = id,
        sock = sock.to_string_lossy(),
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
    }
}

#[test]
fn parent_death_kills_daemon_and_pty_child() {
    let mut d = launch_daemon_under_sh("kill-parent");
    let daemon_pid = d.daemon_pid;
    // Pick up the daemon's descendants BEFORE killing the parent —
    // after orphaning, pgrep -P walks reparent to init and the
    // relationship is lost.
    let pty_children = all_descendants(daemon_pid);
    assert!(
        !pty_children.is_empty(),
        "expected daemon (pid {daemon_pid}) to have at least one PTY descendant; found none",
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
