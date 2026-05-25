//! Test fixture (#310 followup): a fake `calm-session-daemon` that
//! deliberately FAILS the kernel's readiness probe — it never binds
//! the unix socket the kernel polls for. Used to provoke a partial
//! spawn (process is alive + pid persisted + daemon_handle written,
//! but probe times out) so the dispatcher's rollback path has to reap
//! the daemon process before deleting the row.
//!
//! Contract with the kernel:
//!   * Writes its pid to `<sock>.partial-pid` BEFORE entering the
//!     sleep loop, so the test driver can probe whether the rollback
//!     reap actually killed it.
//!   * Does NOT bind `<sock>` — the kernel's `UnixStream::connect`
//!     loop in `spawn_daemon_with_parts` will never succeed and the
//!     spawn helper returns `CalmError::Internal("daemon … did not
//!     become ready")` after ~3s.
//!   * Sleeps forever (well, 5 minutes — generous bound so a leak in
//!     test infra doesn't linger past the suite). The kernel's reap
//!     path is expected to SIGTERM us via the pid in
//!     `terminal_set_pid` long before this deadline.
//!
//! Same `PR_SET_PDEATHSIG` pattern as `argv-recorder-daemon`: if the
//! test process dies, we go with it so a hung reap-test doesn't
//! orphan us beyond the suite.

use std::fs::File;
use std::io::Write;
use std::time::Duration;

fn main() {
    // Same parent-death pattern as `argv-recorder-daemon`.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
        if libc::getppid() == 1 {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }

    let argv: Vec<String> = std::env::args().collect();
    let mut sock_path: Option<String> = None;
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "--sock" && i + 1 < argv.len() {
            sock_path = Some(argv[i + 1].clone());
            break;
        }
        i += 1;
    }
    let sock = sock_path.expect("--sock arg required");

    // Persist our pid to a sidecar file so the test driver can verify
    // the rollback reap actually killed us. We deliberately write this
    // BEFORE the sleep loop so the test driver doesn't race past it.
    let pid_path = format!("{sock}.partial-pid");
    {
        let mut f = File::create(&pid_path).expect("create partial-pid sidecar");
        writeln!(f, "{}", std::process::id()).expect("write pid");
    }

    // CRITICAL: do NOT bind the socket. That's the whole point of this
    // fixture — the kernel's readiness probe must time out, leaving us
    // in the "process alive + handle persisted + probe failed" state
    // that the rollback reap path is supposed to clean up.

    // Sleep with a 5-minute hard cap. The dispatcher's rollback path
    // sends SIGTERM via `reap_terminal_artifacts` → `send_sigterm`,
    // which our default disposition (terminate on SIGTERM) handles.
    // 5 minutes is well past any reasonable test timeout — if we hit
    // it, something else is wrong.
    std::thread::sleep(Duration::from_secs(300));
}
