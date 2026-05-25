//! Test fixture (#310 fix-loop round 4): a fake `calm-session-daemon` that
//! reproduces the **fast-exit success** shape — runs its "command", writes
//! the canonical `<sock>.exit` sidecar with `{"code": 0, "signal_killed":
//! false}`, then exits **without ever binding the socket**. Used to drive
//! the dispatcher's rollback discriminator into the `Preserved` branch
//! (case 2 of `rollback_orphan_worker`).
//!
//! Why a dedicated fixture: the real `calm-session-daemon` writes its
//! exit sidecar only after binding the socket and running its child to
//! completion. The codex/printf-done scenario the dispatcher cares
//! about (daemon spawns, runs `printf done`, exits before kernel's
//! 40ms readiness probe sees the socket) is hard to reproduce
//! deterministically with the real daemon — the probe interval is
//! short enough that wall-clock races make the test flake. This
//! fixture short-circuits the timing: it writes the sidecar directly,
//! skips the socket bind, and exits — guaranteed to land in case 2
//! every run.
//!
//! Contract with the kernel:
//!   * Reads `--sock <path>` from argv (kernel always passes this).
//!   * Writes `<sock>.exit` with `{"code": 0, "signal_killed": false}`
//!     (the same JSON shape `crate::ws::terminal::ExitSidecar`
//!     deserializes).
//!   * Does NOT bind `<sock>` — so the kernel's readiness probe in
//!     `spawn_daemon_with_parts` exhausts its 75×40ms loop and returns
//!     `CalmError::Internal("daemon … did not become ready")`. This is
//!     exactly the spurious error the discriminator is supposed to
//!     swallow when `<sock>.exit` is on disk.
//!   * Exits 0 immediately after writing the sidecar.
//!
//! Same `PR_SET_PDEATHSIG` pattern as the sibling fixtures so an
//! orphaned daemon never lingers past the test suite.

use std::fs::File;
use std::io::Write;

fn main() {
    // Same parent-death pattern as `argv-recorder-daemon` and
    // `never-ready-daemon`. Even though we exit immediately on the
    // happy path, the safety net protects against an early-return
    // panic.
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

    // Write the canonical `.exit` sidecar. JSON shape matches
    // `crate::ws::terminal::ExitSidecar`:
    //   { "code": <i32|null>, "signal_killed": <bool> }
    //
    // `code = 0` + `signal_killed = false` is the "clean exit"
    // shape — the case codex/`printf done`/`/bin/true` would land in.
    let exit_path = format!("{sock}.exit");
    {
        let mut f = File::create(&exit_path).expect("create .exit sidecar");
        // Hand-write the JSON so the fixture doesn't pull in
        // serde_json just for this. The shape is tiny and stable.
        writeln!(f, r#"{{"code":0,"signal_killed":false}}"#).expect("write .exit sidecar");
        f.flush().expect("flush .exit sidecar");
        f.sync_all().expect("sync .exit sidecar");
    }

    // CRITICAL: do NOT bind the socket. The kernel's readiness probe
    // must time out so `spawn_daemon_with_parts` returns Err — that's
    // the spurious error the discriminator is supposed to swallow.
    //
    // Exit 0 right away. The kernel's `tokio::spawn(async { let _ =
    // child.wait().await; })` reaps us cleanly.
}
