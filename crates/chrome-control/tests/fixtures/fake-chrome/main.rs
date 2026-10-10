//! Test fixture: a stand-in for Chrome, started through `Chrome::launch`.
//!
//! It reads its mode from `<user-data-dir>/fake-chrome-mode`:
//! - `serve`: answer every CDP command on fd 3 with a result on fd 4.
//! - `helpers`: first fork three helpers, as Chrome forks its zygotes and
//!   crashpad handlers: one stays in the browser's process group, one moves to
//!   a group of its own, one starts a new session. Their pids go to
//!   `<user-data-dir>/fake-chrome-helpers` as `in_group own_group session`.
//!   Then serve.
//! - `exit:<code>`: exit at once with `code` (a hand-off to a profile holder).
//!
//! Unlike Chrome, it does not exit when the CDP pipe closes, so tests observe
//! `PR_SET_PDEATHSIG` and group signals rather than pipe EOF. Every process
//! here ends by itself after `LIFETIME`, so a failing test leaves nothing for long.
#[cfg(target_os = "linux")]
fn main() {
    fake::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod fake {
    use std::fs::{self, File};
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::FromRawFd;
    use std::path::PathBuf;
    use std::time::Duration;

    const LIFETIME: Duration = Duration::from_secs(20);

    pub fn main() {
        let profile = std::env::args()
            .find_map(|arg| arg.strip_prefix("--user-data-dir=").map(PathBuf::from))
            .expect("--user-data-dir");
        let mode = fs::read_to_string(profile.join("fake-chrome-mode")).expect("mode file");
        let mode = mode.trim();
        if let Some(code) = mode.strip_prefix("exit:") {
            std::process::exit(code.parse().expect("exit code"));
        }
        if mode == "helpers" {
            let pids = [
                helper(|| {}),
                // SAFETY: plain system calls in the forked child.
                helper(|| unsafe {
                    libc::setpgid(0, 0);
                }),
                helper(|| unsafe {
                    libc::setsid();
                }),
            ];
            let list = format!("{} {} {}\n", pids[0], pids[1], pids[2]);
            let staged = profile.join("fake-chrome-helpers.tmp");
            fs::write(&staged, list).unwrap();
            fs::rename(staged, profile.join("fake-chrome-helpers")).unwrap();
        }
        serve();
        std::thread::sleep(LIFETIME);
    }

    /// Forks a helper that runs `detach`, lets go of every inherited pipe and sleeps.
    fn helper(detach: impl FnOnce()) -> i32 {
        // SAFETY: this process is single-threaded here.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                detach();
                // SAFETY: fd juggling on our own descriptors.
                unsafe {
                    let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                    for fd in 0..3 {
                        libc::dup2(null, fd);
                    }
                    libc::close(3);
                    libc::close(4);
                }
                std::thread::sleep(LIFETIME);
                // SAFETY: leave without running the parent's exit handlers.
                unsafe { libc::_exit(0) };
            }
            pid => pid,
        }
    }

    /// Answers CDP commands until the pipe closes.
    fn serve() {
        // SAFETY: Chrome::launch hands us fds 3 (commands) and 4 (responses).
        let commands = unsafe { File::from_raw_fd(3) };
        let mut responses = unsafe { File::from_raw_fd(4) };
        let mut reader = BufReader::new(commands);
        let mut message = Vec::new();
        loop {
            message.clear();
            match reader.read_until(0, &mut message) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            message.pop();
            let request: serde_json::Value = serde_json::from_slice(&message).expect("JSON");
            let reply = serde_json::json!({
                "id": request["id"],
                "result": { "product": "FakeChrome/1.0" },
            });
            let mut bytes = reply.to_string().into_bytes();
            bytes.push(0);
            if responses.write_all(&bytes).is_err() {
                return;
            }
        }
    }
}
