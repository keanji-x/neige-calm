//! Test fixture (#177): a fake `calm-session-daemon` that records its
//! argv to a sidecar file, binds the unix socket, then writes the
//! daemon's deterministic `ready\n` signal to `--ready-fd`.
//!
//! Used by the spawn integration tests to assert that
//! `--terminal-fg=r,g,b --terminal-bg=r,g,b` made it onto the daemon
//! argv from each spawn site. The real daemon binds the same socket
//! and handles `ClientMsg`; this stub just needs to convince the
//! kernel's ready-fd race that the daemon is alive.
//!
//! Argv recording protocol:
//!   * The kernel passes `--sock <path>` like to the real daemon; the
//!     stub reads `<path>` and writes its full argv (one per line) to
//!     `<path>.argv`.
//!   * The kernel passes `--ready-fd <fd>` like to the real daemon; the
//!     stub writes `ready\n` after binding `<path>`.
//!
//! Located via `env!("CARGO_BIN_EXE_argv-recorder-daemon")` from the
//! test crates (see the `[[bin]]` entry in `Cargo.toml`).

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixListener;
use std::time::Duration;

fn main() {
    // #267 — same parent-death pattern as the real daemon. Without
    // this, the stub's blocking `accept()` outlives the test process
    // forever (the 60s deadline below only fires after a connection
    // arrives, and the kernel's spawn-helper closes its `connect()`
    // immediately). Orphans then hold the test binary's inherited
    // stdout pipe, which deadlocks `cargo test`'s stdout consumer
    // (`tail`, CI log forwarder, etc.) for the entire process lifetime.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
        if libc::getppid() == 1 {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }

    let argv: Vec<String> = std::env::args().collect();
    // Find `--sock <path>` so we know where to bind and where to write
    // the sidecar argv file. Find `--ready-fd <fd>` so we can emit the
    // same readiness signal as the real daemon.
    let mut sock_path: Option<String> = None;
    let mut ready_fd: Option<i32> = None;
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "--sock" && i + 1 < argv.len() {
            sock_path = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if argv[i] == "--ready-fd" && i + 1 < argv.len() {
            ready_fd = Some(argv[i + 1].parse().expect("parse --ready-fd"));
            i += 2;
            continue;
        }
        i += 1;
    }
    let sock = sock_path.expect("--sock arg required");
    let ready_fd = ready_fd.expect("--ready-fd arg required");

    // Write argv to sidecar file. One arg per line so the test can
    // assert exact `--terminal-fg=r,g,b` presence without parsing.
    let argv_path = format!("{sock}.argv");
    {
        let mut f = File::create(&argv_path).expect("create argv sidecar");
        for arg in &argv {
            writeln!(f, "{arg}").expect("write argv line");
        }
    }

    // Bind before writing `ready\n`, matching the real daemon's contract.
    let listener = UnixListener::bind(&sock).expect("bind unix socket");
    {
        // SAFETY: `--ready-fd` is an inherited pipe write end owned by
        // this process. Converting it to `File` closes it after the signal.
        let mut ready = unsafe { File::from_raw_fd(ready_fd) };
        let _ = ready.write_all(b"ready\n");
        let _ = ready.flush();
    }

    // Park forever accepting + dropping connections. The spawn helper no
    // longer connects during readiness, but other tests may still probe
    // the socket.
    listener
        .set_nonblocking(false)
        .expect("set blocking accept");

    // Cap total runtime so a stray test stub doesn't linger past the
    // suite. 60s is generous — well past any test deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((_stream, _addr)) => {
                // Immediately drop. The real `ClientMsg` exchange is out
                // of scope for this stub.
            }
            Err(_) => break,
        }
    }
}
