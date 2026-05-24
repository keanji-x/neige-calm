//! Test fixture (#177): a fake `calm-session-daemon` that records its
//! argv to a sidecar file and binds the unix socket the kernel polls
//! for readiness, then idles waiting for connections so the spawn
//! helper's wait-until-socket-ready loop succeeds.
//!
//! Used by the spawn integration tests to assert that
//! `--terminal-fg=r,g,b --terminal-bg=r,g,b` made it onto the daemon
//! argv from each spawn site. The real daemon binds the same socket
//! and handles `ClientMsg`; this stub just needs to convince the
//! kernel's poll loop that the daemon is alive.
//!
//! Argv recording protocol:
//!   * The kernel passes `--sock <path>` like to the real daemon; the
//!     stub reads `<path>` and writes its full argv (one per line) to
//!     `<path>.argv`.
//!
//! Located via `env!("CARGO_BIN_EXE_argv-recorder-daemon")` from the
//! test crates (see the `[[bin]]` entry in `Cargo.toml`).

use std::fs::File;
use std::io::Write;
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
    // the sidecar argv file.
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

    // Write argv to sidecar file. One arg per line so the test can
    // assert exact `--terminal-fg=r,g,b` presence without parsing.
    let argv_path = format!("{sock}.argv");
    {
        let mut f = File::create(&argv_path).expect("create argv sidecar");
        for arg in &argv {
            writeln!(f, "{arg}").expect("write argv line");
        }
    }

    // Bind the socket the kernel is polling for readiness. The kernel's
    // `spawn_daemon_with_parts` calls `UnixStream::connect(&sock)` in a
    // 75-iter / 40ms loop — once we bind here it will succeed and the
    // spawn returns.
    let listener = UnixListener::bind(&sock).expect("bind unix socket");
    // Park forever accepting + dropping connections — the kernel's poll
    // loop only needs `connect()` to succeed.
    listener
        .set_nonblocking(false)
        .expect("set blocking accept");

    // Cap total runtime so a stray test stub doesn't linger past the
    // suite. 60s is generous — well past any test deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        match listener.accept() {
            Ok((_stream, _addr)) => {
                // Immediately drop. The kernel's readiness poll is
                // satisfied by `connect()` returning Ok — it never
                // sends a message. The real `ClientMsg` exchange is
                // out of scope for this stub.
            }
            Err(_) => break,
        }
    }
}
