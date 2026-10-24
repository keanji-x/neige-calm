//! Test fixture: a stand-in for Chrome, started through `Chrome::launch`.
//!
//! It reads its mode from `<user-data-dir>/fake-chrome-mode`:
//! - `serve`: answer every CDP command on fd 3 with a result on fd 4; on
//!   `Browser.close`, answer and exit 0, as Chrome closes in order.
//! - `helpers`: first fork three helpers, as Chrome forks its zygotes and
//!   crashpad handlers: one stays in the browser's process group, one moves to
//!   a group of its own, one starts a new session. Their pids go to
//!   `<user-data-dir>/fake-chrome-helpers` as `in_group own_group session`.
//!   Then serve. `helpers-ignore-close` does the same and serves as
//!   `ignore-close` does.
//!   The in-group helper records a SIGTERM by creating
//!   `<user-data-dir>/in-group-helper-sigterm` before it exits.
//! - `ignore-close`: serve, but answer `Browser.close` without closing.
//! - `ignore-term`: ignore SIGTERM, then serve as `ignore-close` does.
//! - `exit:<code>`: exit at once with `code` (a hand-off to a profile holder).
//!
//! Unlike Chrome, it does not exit when the CDP pipe closes, so tests observe
//! `PR_SET_PDEATHSIG` and group signals rather than pipe EOF. The browser
//! process ends by itself `LIFETIME` after its CDP pipe closes, and each helper
//! `LIFETIME` after it starts, so a failing test leaves nothing for long once
//! its owner is gone.
#[cfg(target_os = "linux")]
fn main() {
    fake::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod fake {
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::fd::FromRawFd;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::time::Duration;

    const LIFETIME: Duration = Duration::from_secs(20);

    /// Path the in-group helper creates when SIGTERM reaches it.
    static SIGTERM_MARKER: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn record_sigterm(_: libc::c_int) {
        // SAFETY: open and _exit are async-signal-safe; the path is a leaked CString.
        unsafe {
            let path = SIGTERM_MARKER.load(Ordering::SeqCst);
            libc::open(path, libc::O_CREAT | libc::O_WRONLY, 0o600);
            libc::_exit(0);
        }
    }

    pub fn main() {
        let profile = std::env::args()
            .find_map(|arg| arg.strip_prefix("--user-data-dir=").map(PathBuf::from))
            .expect("--user-data-dir");
        let mode = fs::read_to_string(profile.join("fake-chrome-mode")).expect("mode file");
        let mode = mode.trim();
        if let Some(code) = mode.strip_prefix("exit:") {
            std::process::exit(code.parse().expect("exit code"));
        }
        if mode == "ignore-term" {
            // SAFETY: plain signal disposition change.
            unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        }
        let mut children = Children(Vec::new());
        if mode.starts_with("helpers") {
            let marker = profile.join("in-group-helper-sigterm");
            let marker = CString::new(marker.into_os_string().into_encoded_bytes()).unwrap();
            SIGTERM_MARKER.store(marker.into_raw(), Ordering::SeqCst);
            // The in-group helper inherits the handler from its first instruction;
            // the browser restores the default right after the fork.
            // SAFETY: plain signal disposition changes.
            unsafe {
                libc::signal(
                    libc::SIGTERM,
                    record_sigterm as extern "C" fn(libc::c_int) as libc::sighandler_t,
                )
            };
            let in_group = helper(&mut children, || Ok(()));
            unsafe { libc::signal(libc::SIGTERM, libc::SIG_DFL) };
            let pids = [
                in_group,
                // SAFETY: plain system calls in the forked child.
                helper(&mut children, || syscall(unsafe { libc::setpgid(0, 0) })),
                helper(&mut children, || syscall(unsafe { libc::setsid() })),
            ];
            let list = format!("{} {} {}\n", pids[0], pids[1], pids[2]);
            let staged = profile.join("fake-chrome-helpers.tmp");
            fs::write(&staged, list).unwrap();
            fs::rename(staged, profile.join("fake-chrome-helpers")).unwrap();
        }
        serve(!mode.ends_with("ignore-close") && mode != "ignore-term");
        std::thread::sleep(LIFETIME);
    }

    /// Own unreaped children during fixture preparation, including on a panic.
    struct Children(Vec<i32>);

    impl Drop for Children {
        fn drop(&mut self) {
            for &pid in &self.0 {
                // SAFETY: these are our unreaped fork children, so their pids
                // cannot have been reused. Signal only a pinned individual.
                unsafe {
                    let fd = libc::syscall(libc::SYS_pidfd_open, pid, 0);
                    if fd >= 0 {
                        libc::syscall(libc::SYS_pidfd_send_signal, fd, libc::SIGKILL, 0, 0);
                        libc::close(fd as i32);
                    }
                    while libc::waitpid(pid, std::ptr::null_mut(), 0) == -1
                        && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
                    {
                    }
                }
            }
        }
    }

    fn syscall(result: i32) -> io::Result<()> {
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Returns the pid only after detach and closing inherited pipes succeed.
    fn helper(children: &mut Children, detach: impl FnOnce() -> io::Result<()>) -> i32 {
        // Each helper has its own close-on-exec readiness pipe.
        let (mut ready, mut ack) = io::pipe().expect("helper readiness pipe");
        // SAFETY: this process is single-threaded here.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", io::Error::last_os_error()),
            0 => {
                drop(ready);
                let prepared = detach().and_then(|()| {
                    // SAFETY: fd juggling on our own descriptors. Close the
                    // original /dev/null too; only the ACK pipe remains.
                    unsafe {
                        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                        syscall(null)?;
                        let result = (|| {
                            for fd in 0..3 {
                                syscall(libc::dup2(null, fd))?;
                            }
                            syscall(libc::close(3))?;
                            syscall(libc::close(4))
                        })();
                        libc::close(null);
                        result
                    }
                });
                // An EOF is failure, never readiness. Do not unwind the
                // parent's child guards in this forked copy on an error.
                if prepared.is_err() || ack.write_all(b"R").is_err() {
                    unsafe { libc::_exit(1) };
                }
                drop(ack);
                std::thread::sleep(LIFETIME);
                // SAFETY: leave without running the parent's exit handlers.
                unsafe { libc::_exit(0) };
            }
            pid => {
                children.0.push(pid);
                drop(ack);
                let mut byte = [0];
                ready.read_exact(&mut byte).expect("helper readiness ACK");
                assert_eq!(&byte, b"R", "helper readiness ACK");
                pid
            }
        }
    }

    /// Answers CDP commands until the pipe closes; `Browser.close` ends the
    /// process when `closes`.
    fn serve(closes: bool) {
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
            if closes && request["method"] == "Browser.close" {
                std::process::exit(0);
            }
        }
    }
}
