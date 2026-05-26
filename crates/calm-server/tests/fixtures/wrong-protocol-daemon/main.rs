//! Test fixture (#337): accepts calm-session probe connections but
//! replies with bytes that are not a `DaemonMsg::ServerHello`.

use std::io::Write;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static TERMINATING: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_signal: libc::c_int) {
    TERMINATING.store(true, Ordering::SeqCst);
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let mut sock_path: Option<String> = None;
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "--sock" && i + 1 < argv.len() {
            sock_path = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        i += 1;
    }
    let sock = sock_path.expect("--sock arg required");
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_sigterm as *const () as libc::sighandler_t,
        );
    }
    let listener = UnixListener::bind(&sock).expect("bind unix socket");
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !TERMINATING.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                let _ = stream.write_all(b"not-a-calm-session-frame");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }

    if TERMINATING.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        let _ = std::fs::remove_file(sock);
    }
}
