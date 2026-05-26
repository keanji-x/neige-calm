//! Test fixture (#349 review followup): write the deterministic
//! `ready\n` signal to `--ready-fd` and then exit immediately.
//!
//! This pins the spawn helper's tie-breaker: if `child.wait()` and the
//! ready pipe are both observable, the buffered ready signal wins.

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;

fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
        if libc::getppid() == 1 {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }

    let argv: Vec<String> = std::env::args().collect();
    let mut ready_fd: Option<i32> = None;
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "--ready-fd" && i + 1 < argv.len() {
            ready_fd = Some(argv[i + 1].parse().expect("parse --ready-fd"));
            break;
        }
        i += 1;
    }
    let ready_fd = ready_fd.expect("--ready-fd arg required");
    {
        // SAFETY: `--ready-fd` is an inherited pipe write end owned by
        // this process. Converting it to `File` closes it after the signal.
        let mut ready = unsafe { File::from_raw_fd(ready_fd) };
        let _ = ready.write_all(b"ready\n");
        let _ = ready.flush();
    }
}
